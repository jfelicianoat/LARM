//! Adaptador declarativo (P023; spec §13; contrato 03; contrato 11 H04;
//! schema `DeclarativeAdapter` y `adapters/runtime/generic/wizard.schema.json`).
//!
//! Un runtime sencillo se describe con datos: ejecutable, argumentos,
//! sondeo de versión, arranque, parada, health HTTP, protocolo, formatos y
//! entorno. LARM lo ejecuta con el worker genérico
//! (`adapters/runtime/generic`), nunca con una cadena de shell.
//!
//! - **Ejecutables**: rutas absolutas y reales de un `.exe` dentro de las
//!   raíces de lectura del manifiesto. Los dos ejecutables del documento son
//!   la allowlist de procesos del adaptador; un formulario no puede añadir
//!   otro. `.bat`/`.cmd` se rechazan: Windows los pasa por `cmd.exe`.
//! - **Placeholders**: `{MODEL_PATH}`, `{PORT}`, `{THREADS}`, `{CONTEXT}` y
//!   `{CONFIG_PATH}` tipados; `{{`/`}}` son llaves literales; uno desconocido
//!   o una llave suelta se rechazan. El valor se inserta tal cual dentro de su
//!   argumento: sin shell, sin variables de entorno y sin reexpansión.
//! - **Entorno** (H04): `EnvironmentConfig` del dominio, con las credenciales
//!   declaradas en `secret_refs` y sin secretos como texto. Para arrancar con
//!   perfil, el binding del perfil sustituye al del mismo nombre y cada
//!   variable efectiva tiene que estar en las dos allowlists.
//! - **Simulación**: [`DeclarativeAdapter::simulate`] expande y comprueba
//!   rutas sin ejecutar nada.
//! - **Paquete**: [`DeclarativeAdapter::materialize`] crea la carpeta del
//!   adaptador (worker genérico + documento + manifiesto con su
//!   `package_sha256` real). Sin firma: `CUSTOM`, confianza `UNTRUSTED`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::launch::{EnvironmentBinding, EnvironmentConfig, LaunchParameters};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::manifest::{is_namespaced, AdapterManifest, Family, MethodTable};
use crate::permissions::{package_hash, Access, Grants, RootMap};

/// `schema_version` del documento.
pub const SCHEMA_VERSION: &str = "1.0";
/// Ejecutable del manifiesto de todo adaptador declarativo.
pub const GENERIC_WORKER: &str = "bin/larm-generic-adapter.exe";
/// Documento declarativo dentro del paquete.
pub const DOCUMENT_FILE: &str = "declarative.json";
/// Manifiesto dentro del paquete (excluido del hash del paquete).
pub const MANIFEST_FILE: &str = "manifest.json";
/// Schema publicado del asistente.
pub const WIZARD_SCHEMA_JSON: &str =
    include_str!("../../../adapters/runtime/generic/wizard.schema.json");

/// Nombres de variable que solo admiten `credential_reference`.
const SECRET_HINTS: [&str; 8] = [
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "API_KEY",
    "APIKEY",
    "CREDENTIAL",
    "PRIVATE_KEY",
];

fn invalid(reason: &str, message: impl Into<String>) -> LarmError {
    LarmError::new(ErrorCode::ValidationFailed, message, reason)
}

fn unsupported(reason: &str, message: impl Into<String>) -> LarmError {
    LarmError::new(ErrorCode::UnsupportedCapability, message, reason).retryable(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ParseMode {
    ExactText,
    JsonField,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionProbe {
    pub executable: String,
    pub args: Vec<String>,
    pub parse_mode: ParseMode,
    pub json_field: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartCommand {
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StopMode {
    Signal,
    Http,
    Rpc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StopSpec {
    pub mode: StopMode,
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthSpec {
    pub url: String,
    pub expected_status: u32,
    pub json_pointer: Option<String>,
    pub expected_value: Option<String>,
}

/// Documento `DeclarativeAdapter` (schema cerrado).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeclarativeAdapter {
    pub schema_version: String,
    pub manifest: AdapterManifest,
    pub version_probe: VersionProbe,
    pub start: StartCommand,
    pub stop: StopSpec,
    pub health: HealthSpec,
    pub protocol_profile_id: String,
    pub formats: Vec<String>,
    pub capabilities: Vec<String>,
    pub environment: EnvironmentConfig,
}

/// Placeholder tipado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Placeholder {
    ModelPath,
    Port,
    Threads,
    Context,
    ConfigPath,
}

impl Placeholder {
    pub const ALL: [Self; 5] = [
        Self::ModelPath,
        Self::Port,
        Self::Threads,
        Self::Context,
        Self::ConfigPath,
    ];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ModelPath => "MODEL_PATH",
            Self::Port => "PORT",
            Self::Threads => "THREADS",
            Self::Context => "CONTEXT",
            Self::ConfigPath => "CONFIG_PATH",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == name)
    }
}

/// Trozo de una plantilla.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    Slot(Placeholder),
}

/// Parsea una plantilla: `{NOMBRE}` es un placeholder; `{{` y `}}`, llaves
/// literales.
///
/// # Errors
/// `VALIDATION_FAILED` `placeholder_unknown` o `placeholder_syntax`.
pub fn parse_template(text: &str) -> Result<Vec<Piece>, LarmError> {
    let mut pieces = Vec::new();
    let mut literal = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                literal.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                literal.push('}');
            }
            '{' => {
                let mut name = String::new();
                let mut closed = false;
                for n in chars.by_ref() {
                    if n == '}' {
                        closed = true;
                        break;
                    }
                    name.push(n);
                }
                if !closed {
                    return Err(invalid(
                        "placeholder_syntax",
                        format!("Llave sin cerrar en «{text}» (usa {{{{ para una llave literal)"),
                    ));
                }
                let slot = Placeholder::parse(&name).ok_or_else(|| {
                    invalid(
                        "placeholder_unknown",
                        format!(
                            "Placeholder desconocido {{{name}}}; solo MODEL_PATH, PORT, THREADS, CONTEXT y CONFIG_PATH"
                        ),
                    )
                })?;
                if !literal.is_empty() {
                    pieces.push(Piece::Text(std::mem::take(&mut literal)));
                }
                pieces.push(Piece::Slot(slot));
            }
            '}' => {
                return Err(invalid(
                    "placeholder_syntax",
                    format!("Llave de cierre suelta en «{text}» (usa }}}} para una llave literal)"),
                ))
            }
            other => literal.push(other),
        }
    }
    if !literal.is_empty() {
        pieces.push(Piece::Text(literal));
    }
    Ok(pieces)
}

fn slots(pieces: &[Piece]) -> impl Iterator<Item = Placeholder> + '_ {
    pieces.iter().filter_map(|p| match p {
        Piece::Slot(s) => Some(*s),
        Piece::Text(_) => None,
    })
}

/// Plantilla que solo admite `allowed`.
fn check_template(text: &str, allowed: &[Placeholder], field: &str) -> Result<(), LarmError> {
    let pieces = parse_template(text)?;
    if let Some(bad) = slots(&pieces).find(|s| !allowed.contains(s)) {
        return Err(invalid(
            "placeholder_not_allowed",
            format!("{{{}}} no se admite en {field}", bad.as_str()),
        ));
    }
    Ok(())
}

/// Valores para expandir placeholders (ya comprobados).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaceholderValues {
    pub model_path: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
    pub port: Option<u16>,
    pub threads: Option<u32>,
    pub context: Option<u32>,
}

impl PlaceholderValues {
    fn get(&self, slot: Placeholder) -> Option<String> {
        match slot {
            Placeholder::ModelPath => self.model_path.as_ref().map(|p| display(p)),
            Placeholder::ConfigPath => self.config_path.as_ref().map(|p| display(p)),
            Placeholder::Port => self.port.map(|p| p.to_string()),
            Placeholder::Threads => self.threads.map(|t| t.to_string()),
            Placeholder::Context => self.context.map(|c| c.to_string()),
        }
    }
}

/// Ruta sin el prefijo `\\?\` de `canonicalize`.
fn display(path: &Path) -> String {
    let s = path.display().to_string();
    s.strip_prefix(r"\\?\").map_or(s.clone(), str::to_string)
}

/// Expande una plantilla ya validada. El valor no se vuelve a interpretar.
///
/// # Errors
/// `VALIDATION_FAILED` `placeholder_value_missing`.
pub fn expand(text: &str, values: &PlaceholderValues) -> Result<String, LarmError> {
    let mut out = String::new();
    for piece in parse_template(text)? {
        match piece {
            Piece::Text(t) => out.push_str(&t),
            Piece::Slot(s) => out.push_str(&values.get(s).ok_or_else(|| {
                invalid(
                    "placeholder_value_missing",
                    format!("Falta el valor de {{{}}}", s.as_str()),
                )
            })?),
        }
    }
    Ok(out)
}

/// `launch_options_schema` de un adaptador declarativo
/// (`LaunchParameters.adapter_options`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchOptions {
    #[serde(default)]
    pub model_path: Option<String>,
    #[serde(default)]
    pub config_path: Option<String>,
    #[serde(default)]
    pub port: Option<u32>,
}

impl LaunchOptions {
    /// # Errors
    /// `VALIDATION_FAILED` `adapter_option_unknown` o `port_invalid`.
    pub fn parse(options: &Map<String, Value>) -> Result<Self, LarmError> {
        let o: Self = serde_json::from_value(Value::Object(options.clone())).map_err(|e| {
            invalid(
                "adapter_option_unknown",
                "adapter_options solo admite model_path, config_path y port",
            )
            .with_technical(e.to_string())
        })?;
        if o.port.is_some_and(|p| !(1..=65535).contains(&p)) {
            return Err(invalid("port_invalid", "port debe estar en 1..65535"));
        }
        Ok(o)
    }
}

/// Orden ya expandido y comprobado (lo que se lanzaría).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Prepared {
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    /// Entorno efectivo: valores no secretos y referencias, nunca secretos.
    pub environment: EnvironmentConfig,
    /// Nombres efectivos (`LaunchPlan.env_allowlist`).
    pub env_allowlist: Vec<String>,
    pub port: Option<u16>,
    pub health_url: Option<String>,
    pub stop_endpoint: Option<String>,
}

/// Resultado de simular sin ejecutar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Simulation {
    pub version_probe: Prepared,
    pub start: Option<Prepared>,
}

/// Paquete creado en disco.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Materialized {
    pub dir: PathBuf,
    pub manifest: AdapterManifest,
}

/// Comprueba que no falte ninguna clave obligatoria anulable (serde trata
/// una ausente como `null`; el schema la exige).
fn require_keys(value: &Value) -> Result<(), LarmError> {
    let groups: [(&str, &[&str]); 5] = [
        ("", &["manifest", "environment"]),
        ("version_probe", &["json_field"]),
        ("stop", &["endpoint"]),
        ("health", &["json_pointer", "expected_value"]),
        ("manifest", &["signature"]),
    ];
    for (parent, keys) in groups {
        let object = if parent.is_empty() {
            value.as_object()
        } else {
            value.get(parent).and_then(Value::as_object)
        };
        let Some(object) = object else {
            continue; // serde informará del objeto ausente.
        };
        for key in keys {
            if !object.contains_key(*key) {
                return Err(invalid(
                    "schema_violation",
                    format!(
                        "Falta {parent}{}{key}",
                        if parent.is_empty() { "" } else { "." }
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn components_ok(path: &str) -> bool {
    Path::new(path).components().all(|c| {
        !matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    })
}

/// Ruta absoluta local de Windows (`X:\…`), sin `..` ni metacaracteres.
fn check_absolute(path: &str, field: &str) -> Result<(), LarmError> {
    let b = path.as_bytes();
    let ok = b.len() > 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'\\' || b[2] == b'/')
        && components_ok(path)
        && !path
            .chars()
            .any(|c| c.is_control() || "<>|&;$`\"'*?%^{}".contains(c));
    if ok {
        Ok(())
    } else {
        Err(invalid(
            "path_invalid",
            format!("{field} debe ser una ruta absoluta X:\\… sin .., variables ni metacaracteres de shell"),
        ))
    }
}

fn check_executable(path: &str, field: &str) -> Result<(), LarmError> {
    check_absolute(path, field).map_err(|_| {
        invalid(
            "executable_invalid",
            format!("{field}: ruta absoluta de un .exe, sin variables ni comandos de shell"),
        )
    })?;
    if !path.to_ascii_lowercase().ends_with(".exe") {
        return Err(invalid(
            "executable_not_exe",
            format!("{field}: solo .exe (un .bat/.cmd se ejecutaría con cmd.exe)"),
        ));
    }
    Ok(())
}

/// URL loopback de health o parada: `http://127.0.0.1|localhost|[::1]:puerto`
/// (o `{PORT}`), sin usuario, query ni fragmento.
fn check_loopback(template: &str, field: &str) -> Result<(), LarmError> {
    check_template(template, &[Placeholder::Port], field)?;
    let probe = expand(
        template,
        &PlaceholderValues {
            port: Some(1),
            ..PlaceholderValues::default()
        },
    )?;
    let bad = |m: &str| invalid("endpoint_invalid", format!("{field}: {m}"));
    let rest = probe
        .strip_prefix("http://")
        .ok_or_else(|| bad("solo http:// de loopback"))?;
    if rest.contains(['?', '#', '@']) {
        return Err(bad(
            "sin usuario, query ni fragmento (la URL no admite tokens como texto)",
        ));
    }
    let authority = rest.split('/').next().unwrap_or_default();
    let (host, port) = authority
        .rsplit_once(':')
        .ok_or_else(|| bad("puerto explícito o {PORT}"))?;
    if !matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
        return Err(LarmError::new(
            ErrorCode::ValidationFailed,
            format!("{field}: solo loopback (127.0.0.1, localhost o [::1])"),
            "endpoint_not_loopback",
        ));
    }
    if !port.parse::<u16>().is_ok_and(|p| p > 0) {
        return Err(bad("puerto 1..65535"));
    }
    Ok(())
}

fn upper_eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

impl DeclarativeAdapter {
    /// Parsea y valida un documento.
    ///
    /// # Errors
    /// `VALIDATION_FAILED` (`schema_violation` u otra regla) o
    /// `UNSUPPORTED_CAPABILITY`.
    pub fn from_value(value: &Value) -> Result<Self, LarmError> {
        require_keys(value)?;
        let doc: Self = serde_json::from_value(value.clone()).map_err(|e| {
            invalid(
                "schema_violation",
                "El documento no cumple el schema DeclarativeAdapter",
            )
            .with_technical(e.to_string())
        })?;
        doc.validate(&MethodTable::contract())?;
        Ok(doc)
    }

    /// # Errors
    /// Ver [`DeclarativeAdapter::from_value`].
    pub fn parse(text: &str) -> Result<Self, LarmError> {
        let value: Value = serde_json::from_str(text).map_err(|e| {
            invalid("schema_violation", "El documento no es JSON").with_technical(e.to_string())
        })?;
        Self::from_value(&value)
    }

    /// Reglas del contrato que el schema no expresa. Puro: no toca el disco.
    ///
    /// # Errors
    /// La primera regla violada.
    #[allow(clippy::too_many_lines)]
    pub fn validate(&self, table: &MethodTable) -> Result<(), LarmError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(invalid(
                "schema_version_invalid",
                "schema_version debe ser 1.0",
            ));
        }
        let m = &self.manifest;
        m.validate(table)?;
        if m.family != Family::Runtime {
            return Err(unsupported(
                "declarative_family_unsupported",
                "El worker genérico solo implementa la familia runtime",
            ));
        }
        if m.executable.relative_path != GENERIC_WORKER || !m.executable.args.is_empty() {
            return Err(invalid(
                "declarative_executable_fixed",
                format!(
                    "El ejecutable de un adaptador declarativo es {GENERIC_WORKER} sin argumentos"
                ),
            ));
        }
        if !m.installation_strategies.is_empty() {
            return Err(unsupported(
                "declarative_installation_unsupported",
                "Un adaptador declarativo usa un ejecutable existente: installation_strategies vacía",
            ));
        }
        if m.signature.is_some() {
            return Err(invalid(
                "declarative_signature_not_supported",
                "Un adaptador declarativo es CUSTOM: signature null (la importación nunca hereda confianza)",
            ));
        }
        let id_ok = {
            let b = m.id.as_bytes();
            !b.is_empty()
                && b.len() <= 64
                && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
                && b.iter().all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
                })
        };
        if !id_ok {
            return Err(invalid(
                "adapter_id_invalid",
                "id: [a-z0-9][a-z0-9._-]{0,63} (es el nombre de su carpeta)",
            ));
        }
        let version_ok = {
            let b = m.version.as_bytes();
            !b.is_empty()
                && b.len() <= 64
                && b[0].is_ascii_alphanumeric()
                && b.iter()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'+' | b'_' | b'-'))
        };
        if !version_ok {
            return Err(invalid(
                "adapter_version_invalid",
                "version: [0-9A-Za-z][0-9A-Za-z.+_-]{0,63}",
            ));
        }
        for root in m
            .permissions
            .read_roots
            .iter()
            .chain(&m.permissions.write_roots)
        {
            check_absolute(root, "raíz de permisos").map_err(|_| {
                invalid(
                    "declarative_root_not_absolute",
                    "Las raíces de un adaptador declarativo son rutas absolutas X:\\…",
                )
            })?;
        }
        // Capacidades: las mismas que anuncia el manifiesto en el handshake.
        let mut caps = BTreeSet::new();
        for c in &self.capabilities {
            if !is_namespaced(c) {
                return Err(invalid(
                    "capability_invalid",
                    format!("Capacidad sin espacio de nombres: {c}"),
                ));
            }
            if !caps.insert(c.as_str()) {
                return Err(invalid("capability_duplicate", format!("{c} repetida")));
            }
        }
        let announced: BTreeSet<&str> = m.capabilities.iter().map(String::as_str).collect();
        if caps != announced {
            return Err(invalid(
                "capabilities_mismatch",
                "capabilities debe coincidir con manifest.capabilities",
            ));
        }
        if self.formats.is_empty() {
            return Err(invalid("formats_empty", "Al menos un formato"));
        }
        let mut formats = BTreeSet::new();
        for f in &self.formats {
            if f.trim().is_empty() || f.trim() != f {
                return Err(invalid("format_invalid", "Formato vacío o con espacios"));
            }
            if !formats.insert(f.to_ascii_uppercase()) {
                return Err(invalid("format_duplicate", format!("{f} repetido")));
            }
        }
        let p = self.protocol_profile_id.as_bytes();
        if p.is_empty()
            || p.len() > 128
            || !(p[0].is_ascii_lowercase() || p[0].is_ascii_digit())
            || !p.iter().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
            })
        {
            return Err(invalid(
                "protocol_profile_invalid",
                "protocol_profile_id: [a-z0-9][a-z0-9._-]{0,127}",
            ));
        }
        // Sondeo de versión: solo lectura, sin placeholders.
        check_executable(&self.version_probe.executable, "version_probe.executable")?;
        for a in &self.version_probe.args {
            check_template(a, &[], "version_probe.args")?;
        }
        match (
            self.version_probe.parse_mode,
            &self.version_probe.json_field,
        ) {
            (ParseMode::ExactText, None) => {}
            (ParseMode::JsonField, Some(f)) if !f.trim().is_empty() => {}
            _ => {
                return Err(invalid(
                    "version_probe_parse_invalid",
                    "JSON_FIELD exige json_field; EXACT_TEXT exige json_field null",
                ))
            }
        }
        // Arranque.
        check_executable(&self.start.executable, "start.executable")?;
        for a in &self.start.args {
            check_template(a, &Placeholder::ALL, "start.args")?;
        }
        check_absolute(&self.start.cwd, "start.cwd")?;
        // Parada.
        match (self.stop.mode, &self.stop.endpoint) {
            (StopMode::Rpc, _) => {
                return Err(unsupported(
                    "stop_mode_rpc_unsupported",
                    "La parada RPC depende del protocolo (P103); el worker genérico admite SIGNAL o HTTP",
                ))
            }
            (StopMode::Signal, None) => {}
            (StopMode::Http, Some(e)) => check_loopback(e, "stop.endpoint")?,
            _ => {
                return Err(invalid(
                    "stop_invalid",
                    "SIGNAL exige endpoint null; HTTP exige endpoint",
                ))
            }
        }
        // Health.
        check_loopback(&self.health.url, "health.url")?;
        if !(100..=599).contains(&self.health.expected_status) {
            return Err(invalid(
                "health_status_invalid",
                "expected_status debe estar en 100..599",
            ));
        }
        match (&self.health.json_pointer, &self.health.expected_value) {
            (None, None) => {}
            (Some(p), Some(_)) if p.starts_with('/') => {}
            _ => {
                return Err(invalid(
                    "health_pointer_invalid",
                    "json_pointer (que empieza por /) y expected_value van juntos o los dos null",
                ))
            }
        }
        self.check_environment(&self.environment)
    }

    /// Reglas de entorno: las comunes (H04) más credenciales declaradas en
    /// el manifiesto y nombres de secreto solo con referencia.
    fn check_environment(&self, env: &EnvironmentConfig) -> Result<(), LarmError> {
        env.validate()
            .map_err(|e| invalid(e.reason, e.detail.clone()))?;
        for binding in &env.variables {
            match binding {
                EnvironmentBinding::Credential {
                    name,
                    credential_reference,
                } => {
                    if !self
                        .manifest
                        .permissions
                        .secret_refs
                        .contains(credential_reference)
                    {
                        return Err(invalid(
                            "credential_reference_not_declared",
                            format!(
                                "{name}: la referencia no está en manifest.permissions.secret_refs"
                            ),
                        ));
                    }
                }
                EnvironmentBinding::Value { name, .. } => {
                    let upper = name.to_ascii_uppercase();
                    if SECRET_HINTS.iter().any(|h| upper.contains(h)) {
                        return Err(invalid(
                            "secret_value_not_allowed",
                            format!("{name} parece un secreto: usa credential_reference (larm/…), nunca el valor"),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Entorno efectivo. Sin perfil, el del adaptador. Con perfil, el
    /// binding del perfil sustituye al del mismo nombre y cada variable
    /// efectiva tiene que estar en las dos allowlists (unir allowlists no
    /// amplía permisos).
    ///
    /// # Errors
    /// `VALIDATION_FAILED` (`env_not_allowed_by_profile`, …).
    pub fn effective_environment(
        &self,
        profile: Option<&EnvironmentConfig>,
    ) -> Result<EnvironmentConfig, LarmError> {
        let Some(profile) = profile else {
            return Ok(self.environment.clone());
        };
        self.check_environment(profile)?;
        let both = |name: &str| {
            self.environment.allowlist.iter().any(|a| upper_eq(a, name))
                && profile.allowlist.iter().any(|a| upper_eq(a, name))
        };
        let mut variables: Vec<EnvironmentBinding> = self
            .environment
            .variables
            .iter()
            .filter(|b| {
                !profile
                    .variables
                    .iter()
                    .any(|p| upper_eq(p.name(), b.name()))
            })
            .cloned()
            .collect();
        variables.extend(profile.variables.iter().cloned());
        for b in &variables {
            if !both(b.name()) {
                return Err(invalid(
                    "env_not_allowed_by_profile",
                    format!(
                        "{} no está autorizada por el adaptador y por el perfil",
                        b.name()
                    ),
                ));
            }
        }
        variables.sort_by(|a, b| a.name().cmp(b.name()));
        let mut allowlist: Vec<String> = self
            .environment
            .allowlist
            .iter()
            .filter(|a| both(a))
            .cloned()
            .collect();
        allowlist.sort();
        Ok(EnvironmentConfig {
            allowlist,
            variables,
        })
    }

    fn grants(&self) -> Grants {
        // Las raíces de un declarativo son absolutas: se resuelven solas.
        Grants::resolve(&self.manifest.permissions, &RootMap::default())
    }

    /// Ruta real dentro de las raíces de lectura (sin ejecutar nada).
    fn real_file(&self, path: &str, field: &str) -> Result<PathBuf, LarmError> {
        check_absolute(path, field)?;
        self.grants()
            .check(&Access {
                reads: vec![PathBuf::from(path)],
                ..Access::default()
            })
            .map_err(|e| e.with_technical(format!("{field} fuera de read_roots")))?;
        let real = std::fs::canonicalize(path).map_err(|_| {
            LarmError::new(
                ErrorCode::ResourceUnavailable,
                format!("{field} no existe"),
                "path_missing",
            )
        })?;
        if !real.is_file() {
            return Err(LarmError::new(
                ErrorCode::ResourceUnavailable,
                format!("{field} no es un fichero"),
                "path_not_file",
            ));
        }
        Ok(real)
    }

    fn real_dir(&self, path: &str, field: &str) -> Result<PathBuf, LarmError> {
        check_absolute(path, field)?;
        self.grants().check(&Access {
            reads: vec![PathBuf::from(path)],
            ..Access::default()
        })?;
        let real = std::fs::canonicalize(path)
            .ok()
            .filter(|p| p.is_dir())
            .ok_or_else(|| {
                LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    format!("{field} no es una carpeta existente"),
                    "path_missing",
                )
            })?;
        Ok(real)
    }

    /// Rutas reales del sondeo, del arranque y de su carpeta de trabajo
    /// dentro de las raíces de lectura (sin ejecutar nada).
    ///
    /// # Errors
    /// `VALIDATION_FAILED` `path_outside_roots`, `RESOURCE_UNAVAILABLE`
    /// `path_missing`/`path_not_file`.
    pub fn check_paths(&self) -> Result<(), LarmError> {
        // En el alta, una ruta fuera de las raíces es un defecto del
        // documento, no una falta de permisos del operador.
        let outside = |e: LarmError| {
            if e.details.reason == "read_denied" {
                invalid(
                    "path_outside_roots",
                    "Ejecutables y carpeta de trabajo deben estar dentro de manifest.permissions.read_roots",
                )
            } else {
                e
            }
        };
        self.real_file(&self.version_probe.executable, "version_probe.executable")
            .map_err(outside)?;
        self.real_file(&self.start.executable, "start.executable")
            .map_err(outside)?;
        self.real_dir(&self.start.cwd, "start.cwd")
            .map_err(outside)?;
        Ok(())
    }

    /// Sondeo de versión expandido y comprobado (entorno del adaptador).
    ///
    /// # Errors
    /// Ejecutable fuera de las raíces o inexistente.
    pub fn prepare_probe(&self) -> Result<Prepared, LarmError> {
        let exe = self.real_file(&self.version_probe.executable, "version_probe.executable")?;
        let environment = self.environment.clone();
        Ok(Prepared {
            executable: display(&exe),
            args: self.version_probe.args.clone(),
            cwd: None,
            env_allowlist: names(&environment),
            environment,
            port: None,
            health_url: None,
            stop_endpoint: None,
        })
    }

    /// Arranque expandido y comprobado para unos parámetros de perfil.
    ///
    /// # Errors
    /// Parámetros inválidos (antes de IO), límites que el worker no puede
    /// respetar (`UNSUPPORTED_CAPABILITY`), rutas fuera de las raíces o
    /// placeholders sin valor.
    pub fn prepare_start(
        &self,
        params: &LaunchParameters,
        max_memory_bytes: Option<&str>,
    ) -> Result<Prepared, LarmError> {
        params
            .validate()
            .map_err(|e| invalid(e.reason, e.detail.clone()))?;
        if !params.runtime_arguments.is_empty() {
            return Err(invalid(
                "runtime_arguments_not_supported",
                "Un adaptador declarativo no publica runtime_arguments_schema: runtime_arguments vacío",
            ));
        }
        let options = LaunchOptions::parse(&params.adapter_options)?;
        if params.memory_limit_bytes.is_some() || max_memory_bytes.is_some() {
            return Err(unsupported(
                "memory_limit_unsupported",
                "El worker genérico no puede imponer un límite de memoria al proceso (Job Object: P130)",
            ));
        }
        let environment = self.effective_environment(Some(&params.environment))?;
        let model_path = options
            .model_path
            .as_deref()
            .map(|p| self.real_file(p, "model_path"))
            .transpose()?;
        let config_path = options
            .config_path
            .as_deref()
            .map(|p| self.real_file(p, "config_path"))
            .transpose()?;
        let values = PlaceholderValues {
            model_path,
            config_path,
            port: options.port.and_then(|p| u16::try_from(p).ok()),
            threads: Some(params.threads),
            context: params.context,
        };
        let exe = self.real_file(&self.start.executable, "start.executable")?;
        let cwd = self.real_dir(&self.start.cwd, "start.cwd")?;
        let args = self
            .start
            .args
            .iter()
            .map(|a| expand(a, &values))
            .collect::<Result<Vec<_>, _>>()?;
        let health_url = expand(&self.health.url, &values)?;
        let stop_endpoint = self
            .stop
            .endpoint
            .as_deref()
            .map(|e| expand(e, &values))
            .transpose()?;
        Ok(Prepared {
            executable: display(&exe),
            args,
            cwd: Some(display(&cwd)),
            env_allowlist: names(&environment),
            environment,
            port: values.port,
            health_url: Some(health_url),
            stop_endpoint,
        })
    }

    /// Simula sondeo y (con parámetros) arranque, sin ejecutar nada.
    ///
    /// # Errors
    /// Los de [`DeclarativeAdapter::prepare_probe`] y
    /// [`DeclarativeAdapter::prepare_start`].
    pub fn simulate(&self, params: Option<&LaunchParameters>) -> Result<Simulation, LarmError> {
        Ok(Simulation {
            version_probe: self.prepare_probe()?,
            start: params.map(|p| self.prepare_start(p, None)).transpose()?,
        })
    }

    /// Exportación: el documento tal cual, que solo contiene valores no
    /// secretos y referencias (la validación lo garantiza).
    ///
    /// # Errors
    /// Documento no serializable.
    pub fn export(&self) -> Result<Value, LarmError> {
        self.validate(&MethodTable::contract())?;
        serde_json::to_value(self).map_err(|e| {
            LarmError::new(
                ErrorCode::Internal,
                "Documento no serializable",
                "serialization",
            )
            .with_technical(e.to_string())
        })
    }

    /// sha256 del RFC 8785 del documento con `package_sha256` a ceros: la
    /// identidad del contenido declarado, independiente del paquete.
    ///
    /// # Errors
    /// Documento no serializable.
    pub fn document_hash(&self) -> Result<String, LarmError> {
        let mut neutral = self.clone();
        neutral.manifest.package_sha256 = "0".repeat(64);
        let value = serde_json::to_value(&neutral).map_err(|e| {
            LarmError::new(
                ErrorCode::Internal,
                "Documento no serializable",
                "serialization",
            )
            .with_technical(e.to_string())
        })?;
        larm_domain::canonical::sha256_hex(&value).map_err(|e| {
            LarmError::new(
                ErrorCode::Internal,
                "Documento no canonicalizable",
                "serialization",
            )
            .with_technical(e)
        })
    }

    /// Crea `<packages_root>/<id>/<version>/` con el worker genérico, el
    /// documento y el manifiesto con su `package_sha256` real. Se prepara en
    /// una carpeta temporal y se renombra al final: nunca queda un paquete a
    /// medias con el nombre definitivo.
    ///
    /// `declarative.json` guarda el documento con `package_sha256` a ceros
    /// (el hash del paquete no puede contenerse a sí mismo); el real está en
    /// `manifest.json`, que el hash excluye.
    ///
    /// # Errors
    /// `CONFLICT` `adapter_version_exists`; `RESOURCE_UNAVAILABLE`
    /// (`generic_worker_missing`, `io_failed`).
    pub fn materialize(
        &self,
        packages_root: &Path,
        worker_exe: &Path,
    ) -> Result<Materialized, LarmError> {
        let io = |what: &str, e: std::io::Error| {
            LarmError::new(
                ErrorCode::ResourceUnavailable,
                "No se pudo crear el paquete del adaptador",
                "io_failed",
            )
            .with_technical(format!("{what}: {e}"))
        };
        if !worker_exe.is_file() {
            return Err(LarmError::new(
                ErrorCode::ResourceUnavailable,
                "Falta el worker genérico de LARM",
                "generic_worker_missing",
            )
            .with_technical(worker_exe.display().to_string()));
        }
        let target = packages_root
            .join(&self.manifest.id)
            .join(&self.manifest.version);
        if target.exists() {
            return Err(LarmError::new(
                ErrorCode::Conflict,
                "Ya existe esa versión del adaptador",
                "adapter_version_exists",
            ));
        }
        let staging = packages_root.join(format!(".staging-{}", uuid::Uuid::now_v7()));
        let result = (|| {
            std::fs::create_dir_all(staging.join("bin")).map_err(|e| io("staging", e))?;
            std::fs::copy(worker_exe, staging.join(GENERIC_WORKER)).map_err(|e| io("worker", e))?;
            let mut neutral = self.clone();
            neutral.manifest.package_sha256 = "0".repeat(64);
            let text = serde_json::to_string_pretty(&neutral).map_err(|e| {
                LarmError::new(
                    ErrorCode::Internal,
                    "Documento no serializable",
                    "serialization",
                )
                .with_technical(e.to_string())
            })?;
            std::fs::write(staging.join(DOCUMENT_FILE), text).map_err(|e| io("documento", e))?;
            let mut manifest = self.manifest.clone();
            manifest.package_sha256 = package_hash(&staging, &[MANIFEST_FILE])?;
            let text = serde_json::to_string_pretty(&manifest).map_err(|e| {
                LarmError::new(
                    ErrorCode::Internal,
                    "Manifiesto no serializable",
                    "serialization",
                )
                .with_technical(e.to_string())
            })?;
            std::fs::write(staging.join(MANIFEST_FILE), text).map_err(|e| io("manifiesto", e))?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| io("carpeta del adaptador", e))?;
            }
            std::fs::rename(&staging, &target).map_err(|e| {
                if target.exists() {
                    LarmError::new(
                        ErrorCode::Conflict,
                        "Ya existe esa versión del adaptador",
                        "adapter_version_exists",
                    )
                } else {
                    io("renombrar", e)
                }
            })?;
            Ok(manifest)
        })();
        match result {
            Ok(manifest) => Ok(Materialized {
                dir: target,
                manifest,
            }),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&staging);
                Err(e)
            }
        }
    }

    /// Carga el documento y el manifiesto de un paquete materializado.
    ///
    /// # Errors
    /// Ficheros ausentes o inválidos.
    pub fn load_package(dir: &Path) -> Result<(Self, AdapterManifest), LarmError> {
        let read = |name: &str| {
            std::fs::read_to_string(dir.join(name)).map_err(|e| {
                LarmError::new(
                    ErrorCode::ResourceUnavailable,
                    "Paquete declarativo incompleto",
                    "io_failed",
                )
                .with_technical(format!("{name}: {e}"))
            })
        };
        let manifest = AdapterManifest::parse(&read(MANIFEST_FILE)?)?;
        let mut doc = Self::parse(&read(DOCUMENT_FILE)?)?;
        doc.manifest
            .package_sha256
            .clone_from(&manifest.package_sha256);
        if doc.manifest != manifest {
            return Err(LarmError::new(
                ErrorCode::IntegrityFailed,
                "El documento y el manifiesto del paquete no coinciden",
                "package_manifest_mismatch",
            ));
        }
        Ok((doc, manifest))
    }
}

fn names(env: &EnvironmentConfig) -> Vec<String> {
    let mut v: Vec<String> = env.variables.iter().map(|b| b.name().to_string()).collect();
    v.sort();
    v
}

/// Versión a partir de la salida del sondeo.
///
/// - `EXACT_TEXT`: stdout recortado (o stderr si stdout está vacío); tiene
///   que ser una sola línea. Varias líneas no se interpretan: la versión es
///   desconocida, nunca la primera línea (que puede ser un aviso).
/// - `JSON_FIELD`: stdout como JSON; `json_field` es un JSON Pointer si
///   empieza por `/` y si no, una clave de primer nivel. Texto o número.
///
/// # Errors
/// `VALIDATION_FAILED` `version_unparseable`.
pub fn parse_version(
    mode: ParseMode,
    json_field: Option<&str>,
    stdout: &str,
    stderr: &str,
) -> Result<String, LarmError> {
    let bad = || {
        LarmError::new(
            ErrorCode::ValidationFailed,
            "No se pudo leer la versión de la salida del sondeo",
            "version_unparseable",
        )
    };
    let version = match mode {
        ParseMode::ExactText => {
            // La salida entera, recortada, es la versión: una línea. Elegir
            // «la primera línea» convertiría un aviso en una versión falsa.
            let whole = |s: &str| {
                let t = s.trim();
                (!t.is_empty()).then(|| t.to_string())
            };
            whole(stdout).or_else(|| whole(stderr)).ok_or_else(bad)?
        }
        ParseMode::JsonField => {
            let value: Value = serde_json::from_str(stdout.trim()).map_err(|_| bad())?;
            let field = json_field.ok_or_else(bad)?;
            let found = if field.starts_with('/') {
                value.pointer(field)
            } else {
                value.get(field)
            };
            match found {
                Some(Value::String(s)) => s.trim().to_string(),
                Some(Value::Number(n)) => n.to_string(),
                _ => return Err(bad()),
            }
        }
    };
    if version.is_empty() || version.chars().count() > 128 || version.chars().any(char::is_control)
    {
        return Err(bad());
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use serde_json::json;

    /// Documento válido con raíces temporales reales.
    pub(crate) fn document(root: &Path) -> Value {
        let exe = root.join("rt").join("fake.exe");
        json!({
            "schema_version": "1.0",
            "manifest": {
                "id": "acme.fake-runtime",
                "version": "1.0.0",
                "family": "runtime",
                "protocol_version": "1.0",
                "publisher": "operador local",
                "executable": {"relative_path": GENERIC_WORKER, "args": []},
                "platforms": ["windows"],
                "architectures": ["x86_64"],
                "methods": MethodTable::contract().methods.iter()
                    .filter(|m| m.required && (m.family == "all" || m.family == "runtime"))
                    .map(|m| m.method.clone()).collect::<Vec<_>>(),
                "permissions": {
                    "read_roots": [root.join("rt").display().to_string(), root.join("models").display().to_string()],
                    "write_roots": [],
                    "network_origins": [],
                    "process_allowlist": [],
                    "secret_refs": ["larm/test/key"]
                },
                "capabilities": ["runtime.text_generate"],
                "package_sha256": "0".repeat(64),
                "signature": null,
                "installation_strategies": []
            },
            "version_probe": {"executable": exe.display().to_string(), "args": ["--version"], "parse_mode": "EXACT_TEXT", "json_field": null},
            "start": {"executable": exe.display().to_string(), "args": ["--model", "{MODEL_PATH}", "--port={PORT}", "-t", "{THREADS}"], "cwd": root.join("rt").display().to_string()},
            "stop": {"mode": "SIGNAL", "endpoint": null},
            "health": {"url": "http://127.0.0.1:{PORT}/health", "expected_status": 200, "json_pointer": "/status", "expected_value": "ok"},
            "protocol_profile_id": "openai.chat-completions",
            "formats": ["GGUF"],
            "capabilities": ["runtime.text_generate"],
            "environment": {"allowlist": ["LARM_TEST"], "variables": [{"name": "LARM_TEST", "value": "ok"}]}
        })
    }

    fn tree() -> tempfile::TempDir {
        // Windows sandbox TEMP can contain braces, which the adapter's path
        // validator correctly rejects. Build the fixture under target without
        // canonicalize(), which would add an extended \\?\ prefix on Windows.
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let base = repo.join("target/larm-adapter-sdk-tests");
        std::fs::create_dir_all(&base).unwrap();
        let dir = tempfile::tempdir_in(base).unwrap();
        std::fs::create_dir_all(dir.path().join("rt")).unwrap();
        std::fs::create_dir_all(dir.path().join("models")).unwrap();
        std::fs::write(dir.path().join("rt").join("fake.exe"), b"MZ").unwrap();
        std::fs::write(dir.path().join("models").join("m.gguf"), b"GGUF").unwrap();
        dir
    }

    fn set(v: &mut Value, pointer: &str, new: Value) {
        *v.pointer_mut(pointer).unwrap() = new;
    }

    fn reason(v: &Value) -> String {
        DeclarativeAdapter::from_value(v)
            .unwrap_err()
            .details
            .reason
    }

    fn params(options: Value, env: Value) -> LaunchParameters {
        let mut p =
            LaunchParameters::builtin("0192aaaa-0000-7000-8000-000000000001", 4, Some(2048), 512);
        p.adapter_options = options.as_object().unwrap().clone();
        p.environment = serde_json::from_value(env).unwrap();
        p
    }

    #[test]
    fn documento_valido_y_roundtrip() {
        let dir = tree();
        let v = document(dir.path());
        let doc = DeclarativeAdapter::from_value(&v).unwrap();
        let exported = doc.export().unwrap();
        assert_eq!(exported, v, "exportar devuelve el mismo documento");
        assert_eq!(DeclarativeAdapter::from_value(&exported).unwrap(), doc);
        // El hash del contenido no depende del orden de claves ni del paquete.
        let mut other = doc.clone();
        other.manifest.package_sha256 = "a".repeat(64);
        assert_eq!(doc.document_hash().unwrap(), other.document_hash().unwrap());
        other.environment.variables = vec![EnvironmentBinding::Value {
            name: "LARM_TEST".into(),
            value: "otro".into(),
        }];
        assert_ne!(doc.document_hash().unwrap(), other.document_hash().unwrap());
    }

    #[test]
    fn schema_cerrado_y_claves_anulables_obligatorias() {
        let dir = tree();
        let mut v = document(dir.path());
        v["extra"] = json!(1);
        assert_eq!(reason(&v), "schema_violation");
        let mut v = document(dir.path());
        v["start"]["env"] = json!({});
        assert_eq!(reason(&v), "schema_violation");
        for (parent, key) in [
            ("version_probe", "json_field"),
            ("stop", "endpoint"),
            ("health", "json_pointer"),
            ("health", "expected_value"),
            ("manifest", "signature"),
        ] {
            let mut v = document(dir.path());
            v[parent].as_object_mut().unwrap().remove(key);
            assert_eq!(reason(&v), "schema_violation", "{parent}.{key}");
        }
        let mut v = document(dir.path());
        v["schema_version"] = json!("2.0");
        assert_eq!(reason(&v), "schema_version_invalid");
    }

    #[test]
    fn manifiesto_del_declarativo() {
        let dir = tree();
        let cases: Vec<(&str, Value, &str)> = vec![
            (
                "/manifest/family",
                json!("source"),
                "method_family_mismatch",
            ),
            (
                "/manifest/executable/relative_path",
                json!("bin/otro.exe"),
                "declarative_executable_fixed",
            ),
            (
                "/manifest/executable/args",
                json!(["--x"]),
                "declarative_executable_fixed",
            ),
            (
                "/manifest/installation_strategies",
                json!(["CUSTOM"]),
                "declarative_installation_unsupported",
            ),
            (
                "/manifest/signature",
                json!({"algorithm": "Ed25519", "key_id": "k", "value": "v"}),
                "declarative_signature_not_supported",
            ),
            ("/manifest/id", json!("../escape"), "adapter_id_invalid"),
            ("/manifest/id", json!("Mayus"), "adapter_id_invalid"),
            (
                "/manifest/version",
                json!("1.0/../x"),
                "adapter_version_invalid",
            ),
            (
                "/manifest/permissions/read_roots",
                json!(["$models"]),
                "declarative_root_not_absolute",
            ),
            (
                "/manifest/methods",
                json!(["core.handshake", "core.cancel", "core.heartbeat"]),
                "method_required_missing",
            ),
            (
                "/capabilities",
                json!(["runtime.otra"]),
                "capabilities_mismatch",
            ),
            (
                "/capabilities",
                json!(["sin_espacio"]),
                "capability_invalid",
            ),
            ("/formats", json!([]), "formats_empty"),
            ("/formats", json!(["GGUF", "gguf"]), "format_duplicate"),
            (
                "/protocol_profile_id",
                json!("OpenAI Chat"),
                "protocol_profile_invalid",
            ),
        ];
        for (pointer, value, expected) in cases {
            let mut v = document(dir.path());
            set(&mut v, pointer, value.clone());
            assert_eq!(reason(&v), expected, "{pointer} = {value}");
        }
    }

    #[test]
    fn ejecutables_y_rutas() {
        let dir = tree();
        let cases: Vec<(&str, Value, &str)> = vec![
            ("/start/executable", json!("fake.exe"), "executable_invalid"),
            (
                "/start/executable",
                json!("C:\\rt\\run.bat"),
                "executable_not_exe",
            ),
            (
                "/start/executable",
                json!("C:\\rt\\run.cmd"),
                "executable_not_exe",
            ),
            (
                "/start/executable",
                json!("C:\\rt\\..\\x.exe"),
                "executable_invalid",
            ),
            (
                "/start/executable",
                json!("%SystemRoot%\\x.exe"),
                "executable_invalid",
            ),
            (
                "/start/executable",
                json!("C:\\rt\\a.exe & calc.exe"),
                "executable_invalid",
            ),
            (
                "/start/executable",
                json!("\\\\srv\\share\\a.exe"),
                "executable_invalid",
            ),
            (
                "/version_probe/executable",
                json!("cmd.exe /c ver"),
                "executable_invalid",
            ),
            ("/start/cwd", json!("relativa"), "path_invalid"),
            ("/start/cwd", json!("C:\\{MODEL_PATH}"), "path_invalid"),
        ];
        for (pointer, value, expected) in cases {
            let mut v = document(dir.path());
            set(&mut v, pointer, value.clone());
            assert_eq!(reason(&v), expected, "{pointer} = {value}");
        }
    }

    #[test]
    fn placeholders_tipados_y_desconocidos() {
        assert_eq!(
            parse_template("--m={MODEL_PATH}x{{y}}").unwrap(),
            vec![
                Piece::Text("--m=".into()),
                Piece::Slot(Placeholder::ModelPath),
                Piece::Text("x{y}".into())
            ]
        );
        let dir = tree();
        let cases: Vec<(&str, Value, &str)> = vec![
            ("/start/args", json!(["{HOME}"]), "placeholder_unknown"),
            (
                "/start/args",
                json!(["{model_path}"]),
                "placeholder_unknown",
            ),
            ("/start/args", json!(["{ PORT }"]), "placeholder_unknown"),
            ("/start/args", json!(["{PORT"]), "placeholder_syntax"),
            ("/start/args", json!(["PORT}"]), "placeholder_syntax"),
            (
                "/version_probe/args",
                json!(["{PORT}"]),
                "placeholder_not_allowed",
            ),
            (
                "/health/url",
                json!("http://127.0.0.1:{PORT}/{MODEL_PATH}"),
                "placeholder_not_allowed",
            ),
        ];
        for (pointer, value, expected) in cases {
            let mut v = document(dir.path());
            set(&mut v, pointer, value.clone());
            assert_eq!(reason(&v), expected, "{pointer} = {value}");
        }
        // Sin shell: `$` y `%` son texto; `${PORT}` es `$` más el puerto y
        // `{{PORT}}` es el texto literal `{PORT}`.
        let mut v = document(dir.path());
        set(&mut v, "/start/args", json!(["${PORT}", "%PATH%{{PORT}}"]));
        let doc = DeclarativeAdapter::from_value(&v).unwrap();
        let values = PlaceholderValues {
            port: Some(9),
            ..PlaceholderValues::default()
        };
        assert_eq!(expand(&doc.start.args[0], &values).unwrap(), "$9");
        assert_eq!(expand(&doc.start.args[1], &values).unwrap(), "%PATH%{PORT}");
    }

    #[test]
    fn health_parada_y_sondeo() {
        let dir = tree();
        let cases: Vec<(&str, Value, &str)> = vec![
            (
                "/health/url",
                json!("https://127.0.0.1:8080/health"),
                "endpoint_invalid",
            ),
            (
                "/health/url",
                json!("http://10.0.0.5:8080/health"),
                "endpoint_not_loopback",
            ),
            (
                "/health/url",
                json!("http://example.com:{PORT}/"),
                "endpoint_not_loopback",
            ),
            (
                "/health/url",
                json!("http://127.0.0.1:{PORT}/h?token=abc"),
                "endpoint_invalid",
            ),
            (
                "/health/url",
                json!("http://u:p@127.0.0.1:{PORT}/"),
                "endpoint_invalid",
            ),
            (
                "/health/url",
                json!("http://127.0.0.1/health"),
                "endpoint_invalid",
            ),
            (
                "/health/expected_status",
                json!(42),
                "health_status_invalid",
            ),
            (
                "/health/json_pointer",
                json!(null),
                "health_pointer_invalid",
            ),
            (
                "/health/json_pointer",
                json!("status"),
                "health_pointer_invalid",
            ),
            (
                "/stop",
                json!({"mode": "RPC", "endpoint": null}),
                "stop_mode_rpc_unsupported",
            ),
            (
                "/stop",
                json!({"mode": "HTTP", "endpoint": null}),
                "stop_invalid",
            ),
            (
                "/stop",
                json!({"mode": "SIGNAL", "endpoint": "http://127.0.0.1:{PORT}/x"}),
                "stop_invalid",
            ),
            (
                "/stop",
                json!({"mode": "HTTP", "endpoint": "http://192.168.1.2:{PORT}/x"}),
                "endpoint_not_loopback",
            ),
            (
                "/version_probe/parse_mode",
                json!("JSON_FIELD"),
                "version_probe_parse_invalid",
            ),
            (
                "/version_probe/json_field",
                json!("version"),
                "version_probe_parse_invalid",
            ),
        ];
        for (pointer, value, expected) in cases {
            let mut v = document(dir.path());
            set(&mut v, pointer, value.clone());
            assert_eq!(reason(&v), expected, "{pointer} = {value}");
        }
        let mut v = document(dir.path());
        set(
            &mut v,
            "/stop",
            json!({"mode": "HTTP", "endpoint": "http://localhost:{PORT}/shutdown"}),
        );
        DeclarativeAdapter::from_value(&v).unwrap();
    }

    #[test]
    fn entorno_h04() {
        let dir = tree();
        // Casos documentales H04-055..058 del paquete más las reglas propias.
        let cases: Vec<(Value, Option<&str>)> = vec![
            (
                json!({"allowlist": ["LARM_TEST"], "variables": [{"name": "LARM_TEST", "value": "ok"}]}),
                None,
            ),
            (
                json!({"allowlist": ["LARM_TEST"], "variables": [{"name": "LARM_TEST", "credential_reference": "larm/test/key"}]}),
                None,
            ),
            (
                json!({"allowlist": ["LARM_TEST"], "variables": [{"name": "LARM_TEST", "credential_reference": "ai-broker/key"}]}),
                Some("schema_or_ref"),
            ),
            (
                json!({"allowlist": ["LARM_TEST", "larm_test"], "variables": []}),
                Some("allowlist_duplicate"),
            ),
            (
                json!({"allowlist": ["LARM_TEST"], "variables": [{"name": "LARM_TEST", "value": "x"}, {"name": "larm_test", "value": "y"}]}),
                Some("env_duplicate"),
            ),
            (
                json!({"allowlist": ["A"], "variables": [{"name": "B", "value": "x"}]}),
                Some("env_not_allowed"),
            ),
            (
                json!({"allowlist": ["1BAD"], "variables": []}),
                Some("env_name_invalid"),
            ),
            (
                json!({"allowlist": ["X"], "variables": [{"name": "X", "credential_reference": "larm/otra"}]}),
                Some("credential_reference_not_declared"),
            ),
            (
                json!({"allowlist": ["HF_TOKEN"], "variables": [{"name": "HF_TOKEN", "value": "hf_abc"}]}),
                Some("secret_value_not_allowed"),
            ),
            (
                json!({"allowlist": ["X"], "variables": [{"name": "X", "value": "a", "credential_reference": "larm/test/key"}]}),
                Some("schema_violation"),
            ),
        ];
        for (env, expected) in cases {
            let mut v = document(dir.path());
            v["environment"] = env.clone();
            match expected {
                None => {
                    DeclarativeAdapter::from_value(&v).unwrap();
                }
                Some("schema_or_ref") => {
                    let r = reason(&v);
                    assert!(
                        r == "credential_reference_invalid" || r == "schema_violation",
                        "{r}"
                    );
                }
                Some(e) => assert_eq!(reason(&v), e, "{env}"),
            }
        }
        // La exportación de un documento con referencia no contiene secretos.
        let mut v = document(dir.path());
        v["environment"] = json!({"allowlist": ["LARM_TEST", "HF_TOKEN"], "variables": [
            {"name": "LARM_TEST", "value": "ok"},
            {"name": "HF_TOKEN", "credential_reference": "larm/test/key"}]});
        let exported = DeclarativeAdapter::from_value(&v)
            .unwrap()
            .export()
            .unwrap();
        let text = exported.to_string();
        assert!(text.contains("larm/test/key") && !text.contains("hf_"));
    }

    #[test]
    fn entorno_efectivo_con_perfil() {
        let dir = tree();
        let mut v = document(dir.path());
        v["environment"] = json!({"allowlist": ["A", "B"], "variables": [{"name": "A", "value": "base"}, {"name": "B", "value": "b"}]});
        let doc = DeclarativeAdapter::from_value(&v).unwrap();
        // Sin perfil: el del adaptador.
        assert_eq!(doc.effective_environment(None).unwrap(), doc.environment);
        // El perfil sustituye A y ambas están en las dos allowlists.
        let profile: EnvironmentConfig = serde_json::from_value(
            json!({"allowlist": ["A", "B"], "variables": [{"name": "A", "value": "perfil"}]}),
        )
        .unwrap();
        let eff = doc.effective_environment(Some(&profile)).unwrap();
        assert_eq!(
            eff.variables,
            vec![
                EnvironmentBinding::Value {
                    name: "A".into(),
                    value: "perfil".into()
                },
                EnvironmentBinding::Value {
                    name: "B".into(),
                    value: "b".into()
                },
            ]
        );
        // Unir allowlists no amplía: B no está en la del perfil.
        let narrow: EnvironmentConfig =
            serde_json::from_value(json!({"allowlist": ["A"], "variables": []})).unwrap();
        assert_eq!(
            doc.effective_environment(Some(&narrow))
                .unwrap_err()
                .details
                .reason,
            "env_not_allowed_by_profile"
        );
        // Ni el perfil puede introducir una variable que el adaptador no admite.
        let wider: EnvironmentConfig = serde_json::from_value(
            json!({"allowlist": ["A", "B", "C"], "variables": [{"name": "C", "value": "x"}]}),
        )
        .unwrap();
        assert_eq!(
            doc.effective_environment(Some(&wider))
                .unwrap_err()
                .details
                .reason,
            "env_not_allowed_by_profile"
        );
    }

    #[test]
    fn simulacion_sin_ejecutar_e_inyeccion() {
        let dir = tree();
        let doc = DeclarativeAdapter::from_value(&document(dir.path())).unwrap();
        let model = dir.path().join("models").join("m.gguf");
        let env = json!({"allowlist": ["LARM_TEST"], "variables": []});
        let sim = doc
            .simulate(Some(&params(
                json!({"model_path": model.display().to_string(), "port": 8123}),
                env.clone(),
            )))
            .unwrap();
        let start = sim.start.unwrap();
        assert_eq!(start.args[0], "--model");
        assert!(start.args[1].to_lowercase().ends_with("m.gguf"));
        assert_eq!(start.args[2..], ["--port=8123", "-t", "4"]);
        assert_eq!(
            start.health_url.as_deref(),
            Some("http://127.0.0.1:8123/health")
        );
        assert_eq!(start.env_allowlist, ["LARM_TEST"]);
        assert_eq!(sim.version_probe.args, ["--version"]);
        // Determinista.
        let again = doc
            .simulate(Some(&params(
                json!({"model_path": model.display().to_string(), "port": 8123}),
                env.clone(),
            )))
            .unwrap();
        assert_eq!(again.start.unwrap(), start);

        // Inyección: un nombre con espacios entra como un único argumento,
        // literal; metacaracteres, llaves, `..` o rutas fuera de las raíces
        // se rechazan antes de expandir, aunque el fichero exista.
        let spaced = dir.path().join("models").join("mi modelo v1.gguf");
        std::fs::write(&spaced, b"GGUF").unwrap();
        let p = params(
            json!({"model_path": spaced.display().to_string(), "port": 8123}),
            env.clone(),
        );
        let s = doc.prepare_start(&p, None).unwrap();
        assert!(s.args[1].ends_with("mi modelo v1.gguf"), "{}", s.args[1]);
        assert_eq!(s.args.len(), 5, "sigue siendo un solo argumento");
        let tricky = dir.path().join("models").join("a & calc.exe {PORT}.gguf");
        std::fs::write(&tricky, b"GGUF").unwrap();
        for bad in [
            tricky.display().to_string(),
            format!("{}\" & calc.exe", model.display()),
            dir.path()
                .join("models")
                .join("..")
                .join("rt")
                .join("fake.exe")
                .display()
                .to_string(),
            "C:\\Windows\\System32\\drivers\\etc\\hosts".to_string(),
        ] {
            let p = params(json!({"model_path": bad, "port": 8123}), env.clone());
            let r = doc.prepare_start(&p, None).unwrap_err().details.reason;
            assert!(
                ["path_invalid", "read_denied"].contains(&r.as_str()),
                "{bad}: {r}"
            );
        }
        // Opciones desconocidas, puerto, args extra, límites y valores ausentes.
        for (options, expected) in [
            (
                json!({"model_path": model.display().to_string(), "port": 8123, "shell": "cmd"}),
                "adapter_option_unknown",
            ),
            (
                json!({"model_path": model.display().to_string(), "port": 70000}),
                "port_invalid",
            ),
            (
                json!({"model_path": model.display().to_string()}),
                "placeholder_value_missing",
            ),
            (json!({"port": 8123}), "placeholder_value_missing"),
        ] {
            let p = params(options, env.clone());
            assert_eq!(
                doc.prepare_start(&p, None).unwrap_err().details.reason,
                expected
            );
        }
        let mut p = params(
            json!({"model_path": model.display().to_string(), "port": 8123}),
            env.clone(),
        );
        p.runtime_arguments = vec!["--model".into(), "C:\\otro.gguf".into()];
        assert_eq!(
            doc.prepare_start(&p, None).unwrap_err().details.reason,
            "runtime_arguments_not_supported"
        );
        let p = params(
            json!({"model_path": model.display().to_string(), "port": 8123}),
            env,
        );
        let e = doc.prepare_start(&p, Some("1024")).unwrap_err();
        assert_eq!(e.code, ErrorCode::UnsupportedCapability);
        assert_eq!(e.details.reason, "memory_limit_unsupported");
    }

    #[test]
    fn version_exacta_y_json() {
        assert_eq!(
            parse_version(ParseMode::ExactText, None, "\n fake 1.2.3 \n", "").unwrap(),
            "fake 1.2.3"
        );
        // Salida real de `ollama --version` 0.40.3 sin servidor: dos avisos;
        // la versión es desconocida, no «Warning: could not connect…».
        let ollama = "Warning: could not connect to a running Ollama instance\nWarning: client version is 0.40.3\n";
        assert_eq!(
            parse_version(ParseMode::ExactText, None, ollama, "")
                .unwrap_err()
                .details
                .reason,
            "version_unparseable"
        );
        assert_eq!(
            parse_version(ParseMode::ExactText, None, "", "v9\n").unwrap(),
            "v9"
        );
        assert_eq!(
            parse_version(
                ParseMode::JsonField,
                Some("version"),
                r#"{"version":"1.2.3"}"#,
                ""
            )
            .unwrap(),
            "1.2.3"
        );
        assert_eq!(
            parse_version(
                ParseMode::JsonField,
                Some("/build/number"),
                r#"{"build":{"number":42}}"#,
                ""
            )
            .unwrap(),
            "42"
        );
        for (mode, field, out) in [
            (ParseMode::ExactText, None, ""),
            (ParseMode::JsonField, Some("version"), "no json"),
            (ParseMode::JsonField, Some("version"), r#"{"v":"1"}"#),
            (
                ParseMode::JsonField,
                Some("version"),
                r#"{"version":{"x":1}}"#,
            ),
        ] {
            assert_eq!(
                parse_version(mode, field, out, "")
                    .unwrap_err()
                    .details
                    .reason,
                "version_unparseable"
            );
        }
    }

    #[test]
    fn materializa_paquete_con_hash_real() {
        let dir = tree();
        let doc = DeclarativeAdapter::from_value(&document(dir.path())).unwrap();
        let worker = dir.path().join("worker.exe");
        std::fs::write(&worker, b"MZ worker").unwrap();
        let root = dir.path().join("adapters");
        assert_eq!(
            doc.materialize(&root, &dir.path().join("no.exe"))
                .unwrap_err()
                .details
                .reason,
            "generic_worker_missing"
        );
        let m = doc.materialize(&root, &worker).unwrap();
        assert_eq!(m.dir, root.join("acme.fake-runtime").join("1.0.0"));
        assert_eq!(
            m.manifest.package_sha256,
            package_hash(&m.dir, &[MANIFEST_FILE]).unwrap()
        );
        let (loaded, manifest) = DeclarativeAdapter::load_package(&m.dir).unwrap();
        assert_eq!(manifest, m.manifest);
        assert_eq!(
            loaded.document_hash().unwrap(),
            doc.document_hash().unwrap()
        );
        // Misma versión: conflicto, sin restos de staging.
        assert_eq!(
            doc.materialize(&root, &worker).unwrap_err().details.reason,
            "adapter_version_exists"
        );
        let leftovers: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".staging"))
            .collect();
        assert!(leftovers.is_empty());
        // Manipular el documento rompe el hash del paquete.
        std::fs::write(
            m.dir.join(DOCUMENT_FILE),
            std::fs::read_to_string(m.dir.join(DOCUMENT_FILE))
                .unwrap()
                .replace("\"ok\"", "\"otro\""),
        )
        .unwrap();
        assert_ne!(
            package_hash(&m.dir, &[MANIFEST_FILE]).unwrap(),
            m.manifest.package_sha256
        );
    }

    #[test]
    fn schema_del_asistente_publicado_y_coherente() {
        let schema: Value = serde_json::from_str(WIZARD_SCHEMA_JSON).unwrap();
        let names: BTreeSet<String> = schema["x-larm-placeholders"]["names"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let code: BTreeSet<String> = Placeholder::ALL
            .iter()
            .map(|p| p.as_str().to_string())
            .collect();
        assert_eq!(names, code);
        assert_eq!(
            schema["allOf"][1]["properties"]["manifest"]["properties"]["executable"]["properties"]
                ["relative_path"]["const"],
            json!(GENERIC_WORKER)
        );
        assert_eq!(
            schema["allOf"][1]["properties"]["stop"]["properties"]["mode"]["enum"],
            json!(["SIGNAL", "HTTP"])
        );
        let options: BTreeSet<String> = schema["$defs"]["LaunchOptions"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let fields: BTreeSet<String> = serde_json::to_value(LaunchOptions::default())
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(options, fields);
    }
}
