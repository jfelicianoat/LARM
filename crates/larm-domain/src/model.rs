//! Modelos, distribuciones, revisiones y procedencia (P040; spec §7, §17,
//! §19, §20, §81; contrato 01).
//!
//! - `CanonicalModel` es la identidad conceptual; solo agrupa con un upstream
//!   original explícito o con igualdad de hashes. Nunca se fusiona por nombre:
//!   la sospecha se publica en `possible_duplicates`.
//! - `ModelDistribution` es la publicación en una fuente, identificada por
//!   `(source_id, upstream_namespace, upstream_model_id)`.
//! - `ModelRevision` conserva la revisión de la fuente. Su clave es
//!   inmutable: un commit, digest o versión, `pending:{UUIDv7}` mientras no
//!   hay pin, o `content:{bundle_hash}` cuando se verifican los bytes.
//! - Formatos, cuantizaciones y capacidades son identificadores extensibles,
//!   no enums cerrados. Una capacidad desconocida se acepta, pero queda
//!   marcada como no cualificada (`REQUIRE_SCHEMA_AND_QUALIFICATION`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{ErrorCode, LarmError};

fn invalid(reason: &str, message: impl Into<String>) -> LarmError {
    LarmError::new(ErrorCode::ValidationFailed, message, reason)
}

/// Capacidades con contrato (`capability-contracts.json`).
pub const KNOWN_CAPABILITIES: [&str; 18] = [
    "text.generate",
    "text.classify",
    "decision.evaluate",
    "embedding.create",
    "rerank",
    "vision.understand",
    "vision.ocr",
    "vision.detect",
    "vision.segment",
    "image.generate",
    "image.upscale",
    "video.generate",
    "audio.transcribe",
    "audio.translate",
    "audio.classify",
    "speech.synthesize",
    "tool.call",
    "generic.inference",
];

/// Alias de solo lectura del contrato: se aceptan y se guardan con su id.
pub const CAPABILITY_ALIASES: [(&str, &str); 7] = [
    ("embedding", "embedding.create"),
    ("vision", "vision.understand"),
    ("ocr", "vision.ocr"),
    ("tensor.infer", "generic.inference"),
    ("audio.synthesize", "speech.synthesize"),
    ("text.translate", "text.generate"),
    ("audio.tagging", "audio.classify"),
];

/// Capacidad normalizada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    pub id: String,
    /// Tiene contrato. Una desconocida no se trata como soportada hasta tener
    /// schema y cualificación.
    pub known: bool,
}

/// Valida y normaliza una capacidad: minúsculas, segmentos `[a-z][a-z0-9_]*`
/// separados por `.`, a lo sumo 128 caracteres; los alias pasan a su id.
///
/// # Errors
/// `VALIDATION_FAILED` `capability_invalid`.
pub fn capability(text: &str) -> Result<Capability, LarmError> {
    let ok = !text.is_empty()
        && text.len() <= 128
        && text.split('.').all(|seg| {
            let mut chars = seg.chars();
            chars.next().is_some_and(|c| c.is_ascii_lowercase())
                && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        });
    if !ok {
        return Err(invalid(
            "capability_invalid",
            format!("Capacidad no válida: {text}"),
        ));
    }
    let id = CAPABILITY_ALIASES
        .iter()
        .find(|(alias, _)| *alias == text)
        .map_or(text, |(_, id)| *id)
        .to_string();
    let known = KNOWN_CAPABILITIES.contains(&id.as_str());
    Ok(Capability { id, known })
}

/// Normaliza una lista de capacidades: ids únicos y ordenados.
///
/// # Errors
/// La primera capacidad no válida.
pub fn capabilities(list: &[String]) -> Result<Vec<String>, LarmError> {
    let mut out: Vec<String> = list
        .iter()
        .map(|c| capability(c).map(|c| c.id))
        .collect::<Result<_, _>>()?;
    out.sort();
    out.dedup();
    Ok(out)
}

/// Identificador extensible de formato, cuantización o arquitectura
/// (`GGUF`, `Q4_K_M`, `SafeTensors`, `q4f16_1`…): 1–64 caracteres
/// `[A-Za-z0-9_.+-]`. Se conserva tal cual; no se inventa ninguno.
///
/// # Errors
/// `VALIDATION_FAILED` `<field>_invalid`.
pub fn identifier(field: &str, text: &str) -> Result<(), LarmError> {
    let ok = !text.is_empty()
        && text.len() <= 64
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '-'));
    if ok {
        Ok(())
    } else {
        Err(invalid(
            &format!("{field}_invalid"),
            format!("{field} no válido: {text}"),
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RevisionState {
    Provisional,
    Candidate,
    Current,
    Retired,
    Rejected,
    Superseded,
}

impl RevisionState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provisional => "PROVISIONAL",
            Self::Candidate => "CANDIDATE",
            Self::Current => "CURRENT",
            Self::Retired => "RETIRED",
            Self::Rejected => "REJECTED",
            Self::Superseded => "SUPERSEDED",
        }
    }
}

/// Tipo de clave de revisión.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionKey {
    /// Commit, digest o versión inmutable de la fuente.
    Upstream(String),
    /// Observación sin pin: `pending:{UUIDv7}`.
    Pending(String),
    /// Bytes verificados: `content:{bundle_hash}`.
    Content(String),
}

fn is_sha256(v: &str) -> bool {
    v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Clasifica y valida una clave de revisión.
///
/// # Errors
/// `VALIDATION_FAILED` `revision_key_invalid`: vacía, con espacios o
/// controles, o un prefijo reservado mal formado.
pub fn revision_key(text: &str) -> Result<RevisionKey, LarmError> {
    let bad = |m: &str| invalid("revision_key_invalid", m.to_string());
    if text.is_empty()
        || text.len() > 256
        || text.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(bad(
            "Clave de revisión vacía, demasiado larga o con espacios",
        ));
    }
    if let Some(id) = text.strip_prefix("pending:") {
        return if crate::bridge::is_uuid_v7(id) {
            Ok(RevisionKey::Pending(id.to_string()))
        } else {
            Err(bad("pending: debe llevar un UUIDv7"))
        };
    }
    if let Some(hash) = text.strip_prefix("content:") {
        return if is_sha256(hash) {
            Ok(RevisionKey::Content(hash.to_string()))
        } else {
            Err(bad("content: debe llevar un sha256 en minúsculas"))
        };
    }
    Ok(RevisionKey::Upstream(text.to_string()))
}

/// `CanonicalModel` (schema `canonical_model`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalModel {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub name: String,
    pub upstream_identity: Option<String>,
    pub capabilities: Vec<String>,
    pub possible_duplicates: Vec<String>,
}

/// `ModelDistribution` (schema `model_distribution`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelDistribution {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub canonical_model_id: String,
    pub source_id: String,
    pub upstream_namespace: String,
    pub upstream_model_id: String,
    pub license_id: Option<String>,
}

/// `ModelRevision` (schema `model_revision`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRevision {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub distribution_id: String,
    pub revision_key: String,
    pub upstream_commit: Option<String>,
    pub manifest_hash: String,
    pub state: RevisionState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Trust {
    Untrusted,
    Declared,
    Verified,
    Revoked,
}

impl Trust {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Untrusted => "UNTRUSTED",
            Self::Declared => "DECLARED",
            Self::Verified => "VERIFIED",
            Self::Revoked => "REVOKED",
        }
    }
}

/// `ArtifactProvenance` (schema `artifact_provenance`). Autor, uploader,
/// repositorio, URL, gated, modelo base, cuantizador y fecha de descarga van
/// en `metadata` con esos nombres (§19); el linaje de conversión, en
/// `parents` + `tool_id`/`tool_version`/`parameters`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactProvenance {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub artifact_id: String,
    pub source_id: Option<String>,
    pub revision_key: Option<String>,
    pub parents: Vec<String>,
    pub tool_id: Option<String>,
    pub tool_version: Option<String>,
    pub parameters: Map<String, Value>,
    pub license_id: Option<String>,
    pub license_hash: Option<String>,
    pub trust: Trust,
    pub actor_id: String,
    pub evidence_refs: Vec<String>,
}

/// Campos de procedencia de §19 que viajan en `metadata`.
pub const PROVENANCE_METADATA: [&str; 8] = [
    "repository",
    "original_author",
    "uploader",
    "download_url",
    "gated",
    "base_model",
    "quantizer",
    "downloaded_at",
];

/// Clase de origen que distingue la interfaz (§19).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OriginClass {
    OfficialPublisher,
    VerifiedDistribution,
    CommunityConversion,
    UnknownOrigin,
}

/// Clase de origen de una procedencia. Sin fuente o con confianza revocada
/// o no fiable: origen desconocido. Con padres (una conversión): comunidad.
/// Verificada y publicada por el autor original: editor oficial; verificada
/// por otro: distribución verificada. Declarada sin verificar nunca sube de
/// comunidad.
#[must_use]
pub fn origin_class(p: &ArtifactProvenance) -> OriginClass {
    if p.source_id.is_none() || matches!(p.trust, Trust::Untrusted | Trust::Revoked) {
        return OriginClass::UnknownOrigin;
    }
    if !p.parents.is_empty() || p.trust == Trust::Declared {
        return OriginClass::CommunityConversion;
    }
    let field = |k: &str| p.metadata.get(k).and_then(Value::as_str);
    match (field("original_author"), field("uploader")) {
        (Some(author), Some(uploader)) if author.eq_ignore_ascii_case(uploader) => {
            OriginClass::OfficialPublisher
        }
        _ => OriginClass::VerifiedDistribution,
    }
}

impl CanonicalModel {
    /// # Errors
    /// Nombre vacío, capacidad no válida, duplicado consigo mismo o no UUIDv7.
    pub fn validate(&self) -> Result<(), LarmError> {
        if self.name.trim().is_empty() {
            return Err(invalid("name_invalid", "El nombre no puede estar vacío"));
        }
        if self
            .upstream_identity
            .as_deref()
            .is_some_and(|u| u.trim().is_empty())
        {
            return Err(invalid(
                "upstream_identity_invalid",
                "upstream_identity vacío",
            ));
        }
        if capabilities(&self.capabilities)? != self.capabilities {
            return Err(invalid(
                "capabilities_not_normalized",
                "Capacidades sin normalizar (ids únicos, ordenados y sin alias)",
            ));
        }
        for d in &self.possible_duplicates {
            if !crate::bridge::is_uuid_v7(d) || *d == self.id {
                return Err(invalid(
                    "possible_duplicate_invalid",
                    "possible_duplicates solo admite otros modelos por UUIDv7",
                ));
            }
        }
        Ok(())
    }
}

impl ModelDistribution {
    /// # Errors
    /// Ids no UUIDv7 o identidad upstream vacía.
    pub fn validate(&self) -> Result<(), LarmError> {
        for (field, value) in [
            ("canonical_model_id", &self.canonical_model_id),
            ("source_id", &self.source_id),
        ] {
            if !crate::bridge::is_uuid_v7(value) {
                return Err(invalid(
                    &format!("{field}_invalid"),
                    format!("{field} no es UUIDv7"),
                ));
            }
        }
        if self.upstream_namespace.trim().is_empty() || self.upstream_model_id.trim().is_empty() {
            return Err(invalid(
                "upstream_identity_invalid",
                "Namespace o id upstream vacío",
            ));
        }
        Ok(())
    }
}

impl ModelRevision {
    /// Una clave `pending:` solo puede estar PROVISIONAL, SUPERSEDED o
    /// REJECTED (no se activa ni se fusiona sin pin); una PROVISIONAL debe
    /// ser `pending:`; `content:` debe coincidir con `manifest_hash`.
    ///
    /// # Errors
    /// `VALIDATION_FAILED` o `INVALID_STATE_TRANSITION` según la regla.
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_sha256(&self.manifest_hash) {
            return Err(invalid(
                "manifest_hash_invalid",
                "manifest_hash no es sha256",
            ));
        }
        if self
            .upstream_commit
            .as_deref()
            .is_some_and(|c| c.trim().is_empty())
        {
            return Err(invalid("upstream_commit_invalid", "upstream_commit vacío"));
        }
        let key = revision_key(&self.revision_key)?;
        let pending = matches!(key, RevisionKey::Pending(_));
        let allowed_pending = matches!(
            self.state,
            RevisionState::Provisional | RevisionState::Superseded | RevisionState::Rejected
        );
        if pending && !allowed_pending {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "Una revisión sin pin no se puede activar",
                "pending_revision_not_activatable",
            ));
        }
        if self.state == RevisionState::Provisional && !pending {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "PROVISIONAL es solo para revisiones pending:",
                "provisional_requires_pending_key",
            ));
        }
        if let RevisionKey::Content(hash) = &key {
            if *hash != self.manifest_hash {
                return Err(invalid(
                    "content_key_mismatch",
                    "content: debe ser el hash del bundle verificado",
                ));
            }
        }
        Ok(())
    }
}

impl ArtifactProvenance {
    /// # Errors
    /// Padres o fuente no UUIDv7, `license_hash` no sha256, actor vacío,
    /// o herramienta sin versión (un linaje de conversión necesita las dos).
    pub fn validate(&self) -> Result<(), LarmError> {
        if self
            .source_id
            .as_deref()
            .is_some_and(|s| !crate::bridge::is_uuid_v7(s))
            || self
                .parents
                .iter()
                .any(|p| !crate::bridge::is_uuid_v7(p) || *p == self.artifact_id)
        {
            return Err(invalid(
                "provenance_reference_invalid",
                "Fuente o padres no válidos",
            ));
        }
        if self.license_hash.as_deref().is_some_and(|h| !is_sha256(h)) {
            return Err(invalid("license_hash_invalid", "license_hash no es sha256"));
        }
        if self.actor_id.trim().is_empty() {
            return Err(invalid("actor_invalid", "La procedencia necesita actor"));
        }
        if self.tool_id.is_some() != self.tool_version.is_some() {
            return Err(invalid(
                "tool_version_required",
                "Herramienta y versión van juntas en el linaje de conversión",
            ));
        }
        Ok(())
    }
}

/// Rol de un artefacto dentro del bundle (P041; spec §21). Se guarda en
/// `model_artifact.metadata.role`; sin rol, `other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRole {
    Weights,
    Tokenizer,
    Processor,
    Mmproj,
    Vae,
    TextEncoder,
    Vocoder,
    Config,
    /// Datos externos de ONNX (`external_data`).
    ExternalData,
    Other,
}

impl ArtifactRole {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Weights => "weights",
            Self::Tokenizer => "tokenizer",
            Self::Processor => "processor",
            Self::Mmproj => "mmproj",
            Self::Vae => "vae",
            Self::TextEncoder => "text_encoder",
            Self::Vocoder => "vocoder",
            Self::Config => "config",
            Self::ExternalData => "external_data",
            Self::Other => "other",
        }
    }
}

/// Confianza normativa de un tamaño de artefacto (P052, contrato 3.1.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ArtifactSizeConfidence {
    Exact,
    High,
    Estimated,
    Unknown,
}

/// Evidencia que respalda el tamaño publicado.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ArtifactSizeSource {
    Manifest,
    ContentLength,
    GitLfsMetadata,
    HttpHead,
    SourceMetadata,
    UserDeclared,
    LocalObservation,
    Unknown,
}

/// Tamaño con confianza, límites, origen y evidencia; bytes se serializa como string decimal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSizeEstimate {
    pub confidence: ArtifactSizeConfidence,
    pub bytes: Option<crate::units::DecimalU64>,
    pub lower_bound_bytes: Option<crate::units::DecimalU64>,
    pub upper_bound_bytes: Option<crate::units::DecimalU64>,
    pub source: ArtifactSizeSource,
    pub evidence_refs: Vec<String>,
    pub reason: Option<String>,
}

impl ArtifactSizeEstimate {
    /// Crea una estimación desconocida sin elevar tamaños históricos sin evidencia.
    #[must_use]
    pub fn unknown(reason: impl Into<String>, evidence_ref: impl Into<String>) -> Self {
        Self {
            confidence: ArtifactSizeConfidence::Unknown,
            bytes: None,
            lower_bound_bytes: None,
            upper_bound_bytes: None,
            source: ArtifactSizeSource::Unknown,
            evidence_refs: vec![evidence_ref.into()],
            reason: Some(reason.into()),
        }
    }

    /// Valida las restricciones semánticas de confianza y cotas del contrato 3.1.0.
    ///
    /// # Errors
    /// Devuelve `VALIDATION_FAILED` si los límites, la fuente o la evidencia no concuerdan.
    pub fn validate(&self) -> Result<(), LarmError> {
        let evidence_valid = !self.evidence_refs.is_empty()
            && self
                .evidence_refs
                .iter()
                .all(|reference| !reference.trim().is_empty());
        let reason_valid = self
            .reason
            .as_deref()
            .is_none_or(|reason| !reason.trim().is_empty());
        let valid = evidence_valid
            && reason_valid
            && match self.confidence {
                ArtifactSizeConfidence::Exact => {
                    self.bytes.is_some()
                        && self.lower_bound_bytes == self.bytes
                        && self.upper_bound_bytes == self.bytes
                        && self.source == ArtifactSizeSource::Manifest
                }
                ArtifactSizeConfidence::High => {
                    self.lower_bound_bytes.is_none()
                        && matches!(
                            (self.bytes, self.upper_bound_bytes),
                            (Some(bytes), Some(upper)) if upper >= bytes
                        )
                        && matches!(
                            self.source,
                            ArtifactSizeSource::ContentLength
                                | ArtifactSizeSource::GitLfsMetadata
                                | ArtifactSizeSource::HttpHead
                                | ArtifactSizeSource::SourceMetadata
                        )
                }
                ArtifactSizeConfidence::Estimated => {
                    self.source != ArtifactSizeSource::Unknown
                        && match (self.bytes, self.lower_bound_bytes, self.upper_bound_bytes) {
                            (Some(bytes), None, Some(upper)) => bytes <= upper,
                            (None, Some(lower), Some(upper)) => lower <= upper,
                            _ => false,
                        }
                }
                ArtifactSizeConfidence::Unknown => {
                    self.bytes.is_none()
                        && self.lower_bound_bytes.is_none()
                        && self.upper_bound_bytes.is_none()
                        && self.source == ArtifactSizeSource::Unknown
                        && self
                            .reason
                            .as_deref()
                            .is_some_and(|reason| !reason.trim().is_empty())
                }
            };
        if valid {
            Ok(())
        } else {
            Err(invalid(
                "artifact_size_estimate_invalid",
                "La confianza, los límites, la fuente o la evidencia del tamaño no concuerdan",
            ))
        }
    }
}

/// `ModelArtifact` (schema `model_artifact`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelArtifact {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub variant_id: String,
    pub filename: String,
    pub format: Option<String>,
    pub quantization: Option<String>,
    pub expected_size: Option<crate::units::DecimalU64>,
    pub actual_size: Option<crate::units::DecimalU64>,
    pub size_estimate: ArtifactSizeEstimate,
    pub sha256: Option<String>,
    pub source_url: Option<String>,
    pub required: bool,
}

impl ModelArtifact {
    /// Rol declarado en `metadata.role` (`other` si falta o no se reconoce).
    #[must_use]
    pub fn role(&self) -> ArtifactRole {
        self.metadata
            .get("role")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or(ArtifactRole::Other)
    }

    /// # Errors
    /// Nombre de fichero no válido como ruta POSIX del bundle, hash no
    /// sha256 o identificadores mal formados.
    pub fn validate(&self) -> Result<(), LarmError> {
        self.size_estimate.validate()?;
        bundle_path(&self.filename)?;
        if self.sha256.as_deref().is_some_and(|h| !is_sha256(h)) {
            return Err(invalid("sha256_invalid", "sha256 no válido"));
        }
        for (field, value) in [
            ("format", &self.format),
            ("quantization", &self.quantization),
        ] {
            if let Some(v) = value {
                identifier(field, v)?;
            }
        }
        if let Some(role) = self.metadata.get("role") {
            if serde_json::from_value::<ArtifactRole>(role.clone()).is_err() {
                return Err(invalid("role_invalid", format!("Rol desconocido: {role}")));
            }
        }
        Ok(())
    }
}

/// `model_variant.model_limits` (schema cerrado).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelLimits {
    pub parameter_count: Option<crate::units::DecimalU64>,
    pub layers: Option<u64>,
    pub kv_heads: Option<u64>,
    pub head_dim: Option<u64>,
    pub context_limit: Option<u64>,
    pub kv_bits: Option<u64>,
    pub model_max_memory_bytes: Option<crate::units::DecimalU64>,
    pub architecture_metadata_hash: Option<String>,
}

/// `ModelVariant` (schema `model_variant`). Ciclo de vida, disponibilidad,
/// promoción, rol y estado se guardan como el texto del contrato y se
/// validan contra sus listas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelVariant {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub revision_id: String,
    pub format: Option<String>,
    pub quantization: Option<String>,
    pub architecture: Option<String>,
    pub bundle_hash: Option<String>,
    pub lifecycle: String,
    pub availability: String,
    pub ownership: crate::artifacts::Ownership,
    pub promotion: String,
    pub system_role: String,
    pub capabilities: Vec<String>,
    pub model_limits: ModelLimits,
    pub system1_state: Option<String>,
    pub update_available: bool,
    pub status: String,
}

/// Etapas persistidas del ciclo de vida de una variante. Los estados de
/// disponibilidad y de revisión no pertenecen a esta máquina.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ModelLifecycle {
    Discovered,
    Selected,
    DownloadPlanned,
    Downloading,
    Downloaded,
    Verified,
    Prepared,
    TechnicallyValidated,
    Available,
    Running,
    Failed,
    Rejected,
    Corrupt,
}

impl ModelLifecycle {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discovered => "DISCOVERED",
            Self::Selected => "SELECTED",
            Self::DownloadPlanned => "DOWNLOAD_PLANNED",
            Self::Downloading => "DOWNLOADING",
            Self::Downloaded => "DOWNLOADED",
            Self::Verified => "VERIFIED",
            Self::Prepared => "PREPARED",
            Self::TechnicallyValidated => "TECHNICALLY_VALIDATED",
            Self::Available => "AVAILABLE",
            Self::Running => "RUNNING",
            Self::Failed => "FAILED",
            Self::Rejected => "REJECTED",
            Self::Corrupt => "CORRUPT",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "DISCOVERED" => Self::Discovered,
            "SELECTED" => Self::Selected,
            "DOWNLOAD_PLANNED" => Self::DownloadPlanned,
            "DOWNLOADING" => Self::Downloading,
            "DOWNLOADED" => Self::Downloaded,
            "VERIFIED" => Self::Verified,
            "PREPARED" => Self::Prepared,
            "TECHNICALLY_VALIDATED" => Self::TechnicallyValidated,
            "AVAILABLE" => Self::Available,
            "RUNNING" => Self::Running,
            "FAILED" => Self::Failed,
            "REJECTED" => Self::Rejected,
            "CORRUPT" => Self::Corrupt,
            _ => return None,
        })
    }
}

/// Disponibilidad derivada de evidencia vigente, independiente del lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ModelAvailability {
    Unknown,
    Offline,
    Incompatible,
    Preparing,
    Available,
    Degraded,
    Corrupt,
}

impl ModelAvailability {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "UNKNOWN",
            Self::Offline => "OFFLINE",
            Self::Incompatible => "INCOMPATIBLE",
            Self::Preparing => "PREPARING",
            Self::Available => "AVAILABLE",
            Self::Degraded => "DEGRADED",
            Self::Corrupt => "CORRUPT",
        }
    }
}

/// Hechos que deben quedar registrados por la operación que solicita una
/// transición. Ningún campo se infiere de metadata libre.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LifecycleEvidence {
    pub selection_recorded: bool,
    pub preflight_valid: bool,
    pub job_admitted: bool,
    pub all_bytes_present: bool,
    pub hashes_and_bundle_verified: bool,
    pub preparation_recorded: bool,
    pub technical_validation_passed: bool,
    pub hard_gates_passed: bool,
    pub ready_instance_with_lease: bool,
    pub no_active_instances: bool,
    pub failure_cause_persisted: bool,
    pub rejection_recorded: bool,
    pub corruption_confirmed: bool,
    pub reverification_passed: bool,
    pub last_safe_state: Option<ModelLifecycle>,
    pub previous_attempt: u64,
    pub next_attempt: u64,
}

/// Rechaza cualquier transición que no esté enumerada en el contrato o que
/// carezca de su evidencia correspondiente.
///
/// # Errors
/// `INVALID_STATE_TRANSITION` con HTTP 409.
pub fn validate_lifecycle_transition(
    from: ModelLifecycle,
    to: ModelLifecycle,
    evidence: &LifecycleEvidence,
) -> Result<(), LarmError> {
    let valid = match (from, to) {
        (ModelLifecycle::Discovered, ModelLifecycle::Selected) => evidence.selection_recorded,
        (ModelLifecycle::Selected, ModelLifecycle::DownloadPlanned) => evidence.preflight_valid,
        (ModelLifecycle::DownloadPlanned, ModelLifecycle::Downloading) => evidence.job_admitted,
        (ModelLifecycle::Downloading, ModelLifecycle::Downloaded) => evidence.all_bytes_present,
        (ModelLifecycle::Downloaded, ModelLifecycle::Verified) => {
            evidence.hashes_and_bundle_verified
        }
        (ModelLifecycle::Verified, ModelLifecycle::Prepared) => evidence.preparation_recorded,
        (ModelLifecycle::Prepared, ModelLifecycle::TechnicallyValidated) => {
            evidence.technical_validation_passed
        }
        (ModelLifecycle::TechnicallyValidated, ModelLifecycle::Available) => {
            evidence.hard_gates_passed
        }
        (ModelLifecycle::Available, ModelLifecycle::Running) => evidence.ready_instance_with_lease,
        (ModelLifecycle::Running, ModelLifecycle::Available) => evidence.no_active_instances,
        (ModelLifecycle::Discovered | ModelLifecycle::Selected, ModelLifecycle::Rejected) => {
            evidence.rejection_recorded
        }
        (
            ModelLifecycle::Selected
            | ModelLifecycle::DownloadPlanned
            | ModelLifecycle::Downloading
            | ModelLifecycle::Downloaded
            | ModelLifecycle::Verified
            | ModelLifecycle::Prepared
            | ModelLifecycle::TechnicallyValidated,
            ModelLifecycle::Failed,
        ) => evidence.failure_cause_persisted,
        (ModelLifecycle::Failed, safe_state) => {
            evidence.last_safe_state == Some(safe_state)
                && safe_state != ModelLifecycle::Failed
                && safe_state != ModelLifecycle::Corrupt
                && safe_state != ModelLifecycle::Running
                && safe_state != ModelLifecycle::Rejected
                && evidence.next_attempt > evidence.previous_attempt
        }
        (state, ModelLifecycle::Corrupt) if state != ModelLifecycle::Corrupt => {
            evidence.corruption_confirmed
        }
        (ModelLifecycle::Corrupt, ModelLifecycle::Verified) => evidence.reverification_passed,
        _ => false,
    };

    if valid {
        Ok(())
    } else {
        Err(LarmError::new(
            ErrorCode::InvalidStateTransition,
            format!(
                "Transición no permitida: {} -> {}",
                from.as_str(),
                to.as_str()
            ),
            "invalid_state_transition",
        ))
    }
}

/// Valores del contrato para los campos de texto de la variante.
pub const LIFECYCLE: [&str; 13] = [
    "DISCOVERED",
    "SELECTED",
    "DOWNLOAD_PLANNED",
    "DOWNLOADING",
    "DOWNLOADED",
    "VERIFIED",
    "PREPARED",
    "TECHNICALLY_VALIDATED",
    "AVAILABLE",
    "RUNNING",
    "FAILED",
    "REJECTED",
    "CORRUPT",
];
pub const AVAILABILITY: [&str; 7] = [
    "UNKNOWN",
    "OFFLINE",
    "INCOMPATIBLE",
    "PREPARING",
    "AVAILABLE",
    "DEGRADED",
    "CORRUPT",
];
pub const PROMOTION: [&str; 7] = [
    "NONE",
    "CANDIDATE",
    "TECHNICALLY_APPROVED",
    "EVALUATION_APPROVED",
    "DEFAULT",
    "PINNED",
    "REJECTED",
];
pub const SYSTEM_ROLE: [&str; 3] = ["GENERAL", "SYSTEM1", "AUXILIARY"];

/// Máquina de estados de promoción de variantes (P120): escalón ascendente
/// estricto más las salidas laterales REJECTED/PINNED. `DEFAULT` sólo se
/// alcanza desde `EVALUATION_APPROVED`; para cambiar de `DEFAULT` se promueve
/// otra variante (la anterior retrocede en la misma transacción). Jamás se
/// llega a `REJECTED` desde `DEFAULT`.
#[must_use]
pub fn can_transition_promotion(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("NONE", "CANDIDATE")
            | ("CANDIDATE", "TECHNICALLY_APPROVED")
            | ("TECHNICALLY_APPROVED", "EVALUATION_APPROVED")
            | ("EVALUATION_APPROVED", "DEFAULT")
            | ("NONE", "REJECTED")
            | ("CANDIDATE", "REJECTED")
            | ("TECHNICALLY_APPROVED", "REJECTED")
            | ("EVALUATION_APPROVED", "REJECTED")
            | ("NONE", "PINNED")
            | ("CANDIDATE", "PINNED")
            | ("TECHNICALLY_APPROVED", "PINNED")
            | ("EVALUATION_APPROVED", "PINNED")
            | ("DEFAULT", "PINNED")
    )
}

/// Siguiente estado legal de promoción para presentar en la interfaz.
#[must_use]
pub fn next_promotion(from: &str) -> Option<&'static str> {
    match from {
        "NONE" => Some("CANDIDATE"),
        "CANDIDATE" => Some("TECHNICALLY_APPROVED"),
        "TECHNICALLY_APPROVED" => Some("EVALUATION_APPROVED"),
        "EVALUATION_APPROVED" => Some("DEFAULT"),
        _ => None,
    }
}

pub const SYSTEM1_STATE: [&str; 5] = [
    "SYSTEM1_CANDIDATE",
    "SYSTEM1_TECHNICALLY_VALID",
    "SYSTEM1_EVALUATED",
    "SYSTEM1_CERTIFIED",
    "SYSTEM1_REJECTED",
];
pub const STATUS: [&str; 15] = [
    "DISCOVERED",
    "SELECTED",
    "DOWNLOAD_PLANNED",
    "DOWNLOADING",
    "DOWNLOADED",
    "VERIFIED",
    "PREPARED",
    "TECHNICALLY_VALIDATED",
    "AVAILABLE",
    "RUNNING",
    "OFFLINE",
    "CORRUPT",
    "UPDATE_AVAILABLE",
    "FAILED",
    "REJECTED",
];

impl ModelVariant {
    /// Deriva el status de lectura conforme a §116. No altera lifecycle ni
    /// considera `update_available` una razón para invalidar la variante.
    #[must_use]
    pub fn derived_status(&self) -> &'static str {
        Self::status_for(&self.lifecycle, &self.availability, self.update_available)
    }

    #[must_use]
    pub fn status_for(lifecycle: &str, availability: &str, update_available: bool) -> &'static str {
        if lifecycle == "CORRUPT" || availability == "CORRUPT" {
            "CORRUPT"
        } else if availability == "OFFLINE" {
            "OFFLINE"
        } else if lifecycle == "FAILED" {
            "FAILED"
        } else if lifecycle == "REJECTED" {
            "REJECTED"
        } else if lifecycle == "RUNNING" {
            "RUNNING"
        } else if update_available {
            "UPDATE_AVAILABLE"
        } else {
            // El fallback sólo se usa después de validar lifecycle contra el
            // contrato. Esta función también se usa para lecturas heredadas.
            LIFECYCLE
                .iter()
                .copied()
                .find(|state| *state == lifecycle)
                .unwrap_or("DISCOVERED")
        }
    }

    /// # Errors
    /// Valor fuera del contrato, identificador mal formado, capacidades sin
    /// normalizar o hash no sha256.
    pub fn validate(&self) -> Result<(), LarmError> {
        let check = |field: &str, value: &str, allowed: &[&str]| {
            if allowed.contains(&value) {
                Ok(())
            } else {
                Err(invalid(
                    &format!("{field}_invalid"),
                    format!("{field} fuera del contrato: {value}"),
                ))
            }
        };
        check("lifecycle", &self.lifecycle, &LIFECYCLE)?;
        check("availability", &self.availability, &AVAILABILITY)?;
        check("promotion", &self.promotion, &PROMOTION)?;
        check("system_role", &self.system_role, &SYSTEM_ROLE)?;
        check("status", &self.status, &STATUS)?;
        if self.status != self.derived_status() {
            return Err(invalid(
                "status_not_derived",
                "status no coincide con lifecycle, availability y update_available",
            ));
        }
        if let Some(s) = &self.system1_state {
            check("system1_state", s, &SYSTEM1_STATE)?;
        }
        for (field, value) in [
            ("format", &self.format),
            ("quantization", &self.quantization),
            ("architecture", &self.architecture),
        ] {
            if let Some(v) = value {
                identifier(field, v)?;
            }
        }
        let hashes = [
            self.bundle_hash.as_deref(),
            self.model_limits.architecture_metadata_hash.as_deref(),
        ];
        if hashes.into_iter().flatten().any(|h| !is_sha256(h)) {
            return Err(invalid("hash_invalid", "Hash no sha256 en la variante"));
        }
        if capabilities(&self.capabilities)? != self.capabilities {
            return Err(invalid(
                "capabilities_not_normalized",
                "Capacidades sin normalizar",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BundleState {
    Incomplete,
    Verified,
    Corrupt,
}

impl BundleState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Incomplete => "INCOMPLETE",
            Self::Verified => "VERIFIED",
            Self::Corrupt => "CORRUPT",
        }
    }
}

/// `ModelBundle` (schema `model_bundle`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelBundle {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub row_version: i64,
    pub metadata: Map<String, Value>,
    pub variant_id: String,
    pub required_artifact_ids: Vec<String>,
    pub optional_artifact_ids: Vec<String>,
    pub manifest_hash: String,
    pub state: BundleState,
}

/// Descriptor de un fichero del bundle (contrato 01).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleEntry {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub required: bool,
}

/// Ruta relativa POSIX del bundle: separador `/`, sin absolutos, unidad,
/// ADS, `.`/`..` ni componentes vacíos.
///
/// # Errors
/// `VALIDATION_FAILED` `bundle_path_invalid`.
pub fn bundle_path(path: &str) -> Result<(), LarmError> {
    let bad = || {
        invalid(
            "bundle_path_invalid",
            format!("Ruta de bundle no válida: {path}"),
        )
    };
    if path.contains('\\') {
        return Err(bad());
    }
    match crate::artifacts::normalize_relative(path) {
        Ok(normal) if normal == path => Ok(()),
        _ => Err(bad()),
    }
}

/// Hash canónico (RFC 8785 + sha256) de la lista ordenada de descriptores.
/// Rechaza rutas no válidas, hashes no sha256, tamaños fuera del rango
/// exacto de JSON y rutas duplicadas sin distinguir mayúsculas.
///
/// # Errors
/// `VALIDATION_FAILED` con el motivo.
pub fn bundle_hash(entries: &[BundleEntry]) -> Result<String, LarmError> {
    let mut sorted = entries.to_vec();
    sorted.sort();
    let mut seen = std::collections::BTreeSet::new();
    for e in &sorted {
        bundle_path(&e.relative_path)?;
        if !is_sha256(&e.sha256) {
            return Err(invalid(
                "bundle_sha256_invalid",
                format!("sha256 no válido en {}", e.relative_path),
            ));
        }
        if e.size_bytes > (1u64 << 53) {
            return Err(invalid(
                "bundle_size_invalid",
                "Tamaño fuera del rango exacto de JSON",
            ));
        }
        if !seen.insert(e.relative_path.to_lowercase()) {
            return Err(invalid(
                "bundle_path_duplicate",
                format!(
                    "Ruta duplicada sin distinguir mayúsculas: {}",
                    e.relative_path
                ),
            ));
        }
    }
    let value =
        serde_json::to_value(&sorted).map_err(|e| invalid("bundle_serialize", e.to_string()))?;
    crate::canonical::sha256_hex(&value).map_err(|e| invalid("bundle_canonical", e))
}

/// Hash de identidad de la variante: el canónico de sus descriptores
/// requeridos (contrato 01: `canonical_hash de required artifact descriptors`).
///
/// # Errors
/// Los de [`bundle_hash`].
pub fn required_hash(entries: &[BundleEntry]) -> Result<String, LarmError> {
    let required: Vec<BundleEntry> = entries.iter().filter(|e| e.required).cloned().collect();
    bundle_hash(&required)
}

/// Lo observado en disco de un fichero del bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub sha256: String,
    pub size_bytes: u64,
}

/// Estado del bundle frente a lo observado: un fichero con otros bytes es
/// CORRUPT; si falta alguno requerido (un shard, el tokenizador…),
/// INCOMPLETE; si todos los requeridos coinciden, VERIFIED. Un opcional
/// ausente no impide VERIFIED.
#[must_use]
pub fn bundle_state(
    entries: &[BundleEntry],
    observed: &std::collections::BTreeMap<String, Observed>,
) -> BundleState {
    let mut missing = false;
    for e in entries {
        match observed.get(&e.relative_path) {
            Some(o) if o.sha256 == e.sha256 && o.size_bytes == e.size_bytes => {}
            Some(_) => return BundleState::Corrupt,
            None if e.required => missing = true,
            None => {}
        }
    }
    if missing {
        BundleState::Incomplete
    } else {
        BundleState::Verified
    }
}

/// Roles que una variante necesita para ejecutarse según su formato y sus
/// capacidades (§21: la compatibilidad se calcula sobre el bundle). `None`
/// si el formato no se conoce: no se inventa un requisito.
#[must_use]
pub fn required_roles(format: Option<&str>, capabilities: &[String]) -> Option<Vec<ArtifactRole>> {
    let has = |c: &str| capabilities.iter().any(|x| x == c);
    let lower = format.map(str::to_ascii_lowercase);
    let mut roles = match lower.as_deref() {
        // GGUF lleva el tokenizador y la configuración dentro del fichero.
        Some("gguf" | "onnx") => vec![ArtifactRole::Weights],
        Some("safetensors" | "transformers" | "pytorch") => vec![
            ArtifactRole::Weights,
            ArtifactRole::Config,
            ArtifactRole::Tokenizer,
        ],
        Some("diffusers") => vec![
            ArtifactRole::Weights,
            ArtifactRole::Vae,
            ArtifactRole::TextEncoder,
            ArtifactRole::Config,
        ],
        _ => return None,
    };
    if has("vision.understand") && lower.as_deref() == Some("gguf") {
        roles.push(ArtifactRole::Mmproj);
    }
    if has("speech.synthesize") {
        roles.push(ArtifactRole::Vocoder);
    }
    roles.sort();
    roles.dedup();
    Some(roles)
}

/// Roles necesarios que faltan entre los artefactos requeridos de la
/// variante. `None` si el formato no se conoce.
#[must_use]
pub fn missing_roles(
    format: Option<&str>,
    capabilities: &[String],
    artifacts: &[ModelArtifact],
) -> Option<Vec<ArtifactRole>> {
    let present: std::collections::BTreeSet<ArtifactRole> = artifacts
        .iter()
        .filter(|a| a.required)
        .map(ModelArtifact::role)
        .collect();
    required_roles(format, capabilities)
        .map(|need| need.into_iter().filter(|r| !present.contains(r)).collect())
}

#[cfg(test)]
mod bundle_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use std::collections::BTreeMap;

    fn entry(path: &str, fill: char, size: u64, required: bool) -> BundleEntry {
        BundleEntry {
            relative_path: path.into(),
            sha256: fill.to_string().repeat(64),
            size_bytes: size,
            required,
        }
    }

    fn shards() -> Vec<BundleEntry> {
        vec![
            entry("model-00001-of-00002.gguf", 'a', 10, true),
            entry("model-00002-of-00002.gguf", 'b', 20, true),
            entry("mmproj-f16.gguf", 'c', 5, true),
            entry("README.md", 'd', 1, false),
        ]
    }

    #[test]
    fn hash_determinista_independiente_del_orden() {
        let a = bundle_hash(&shards()).unwrap();
        let mut reversed = shards();
        reversed.reverse();
        assert_eq!(a, bundle_hash(&reversed).unwrap());
        // Cualquier cambio en un descriptor cambia el hash.
        let mut size = shards();
        size[0].size_bytes = 11;
        assert_ne!(a, bundle_hash(&size).unwrap());
        let mut req = shards();
        req[3].required = true;
        assert_ne!(a, bundle_hash(&req).unwrap());
        // El hash de identidad solo mira los requeridos.
        let mut readme = shards();
        readme[3].sha256 = "e".repeat(64);
        assert_eq!(
            required_hash(&shards()).unwrap(),
            required_hash(&readme).unwrap()
        );
        assert_ne!(
            bundle_hash(&shards()).unwrap(),
            bundle_hash(&readme).unwrap()
        );
    }

    #[test]
    fn rutas_y_duplicados() {
        for bad in ["a\\b.gguf", "/abs.gguf", "../x", "C:/x", "a//b", "a:ads"] {
            let mut e = shards();
            e[0].relative_path = bad.into();
            assert_eq!(
                bundle_hash(&e).unwrap_err().details.reason,
                "bundle_path_invalid",
                "{bad}"
            );
        }
        let mut dup = shards();
        dup[3].relative_path = "MODEL-00001-of-00002.GGUF".into();
        assert_eq!(
            bundle_hash(&dup).unwrap_err().details.reason,
            "bundle_path_duplicate"
        );
        let mut sha = shards();
        sha[0].sha256 = "X".repeat(64);
        assert_eq!(
            bundle_hash(&sha).unwrap_err().details.reason,
            "bundle_sha256_invalid"
        );
        let mut big = shards();
        big[0].size_bytes = (1 << 53) + 1;
        assert!(bundle_hash(&big).is_err());
    }

    #[test]
    fn estado_del_bundle() {
        let entries = shards();
        let all: BTreeMap<String, Observed> = entries
            .iter()
            .filter(|e| e.required)
            .map(|e| {
                (
                    e.relative_path.clone(),
                    Observed {
                        sha256: e.sha256.clone(),
                        size_bytes: e.size_bytes,
                    },
                )
            })
            .collect();
        assert_eq!(
            bundle_state(&entries, &all),
            BundleState::Verified,
            "el opcional no hace falta"
        );
        let mut missing = all.clone();
        missing.remove("model-00002-of-00002.gguf");
        assert_eq!(
            bundle_state(&entries, &missing),
            BundleState::Incomplete,
            "falta un shard"
        );
        let mut corrupt = all;
        corrupt.get_mut("mmproj-f16.gguf").unwrap().sha256 = "f".repeat(64);
        assert_eq!(bundle_state(&entries, &corrupt), BundleState::Corrupt);
    }

    fn artifact(role: &str, required: bool) -> ModelArtifact {
        let mut metadata = Map::new();
        metadata.insert("role".into(), role.into());
        ModelArtifact {
            id: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            row_version: 1,
            metadata,
            variant_id: String::new(),
            filename: "f".into(),
            format: None,
            quantization: None,
            expected_size: None,
            actual_size: None,
            size_estimate: ArtifactSizeEstimate::unknown(
                "La prueba no aporta evidencia de confianza del tamaño",
                "test:artifact-size-unknown",
            ),
            sha256: None,
            source_url: None,
            required,
        }
    }

    #[test]
    fn artifact_size_estimate_preserves_confidence_and_decimal_bytes() {
        let exact = ArtifactSizeEstimate {
            confidence: ArtifactSizeConfidence::Exact,
            bytes: crate::units::DecimalU64::new(4_294_967_296),
            lower_bound_bytes: crate::units::DecimalU64::new(4_294_967_296),
            upper_bound_bytes: crate::units::DecimalU64::new(4_294_967_296),
            source: ArtifactSizeSource::Manifest,
            evidence_refs: vec!["manifest:sha256:abc".into()],
            reason: None,
        };
        exact.validate().unwrap();
        let serialized = serde_json::to_value(&exact).unwrap();
        assert_eq!(serialized["confidence"], "EXACT");
        assert_eq!(serialized["bytes"], "4294967296");
        assert_eq!(
            serde_json::from_value::<ArtifactSizeEstimate>(serialized).unwrap(),
            exact
        );

        let unknown = ArtifactSizeEstimate::unknown(
            "La fila antigua no contiene evidencia de confianza",
            "migration:0006",
        );
        unknown.validate().unwrap();
        assert_eq!(unknown.confidence, ArtifactSizeConfidence::Unknown);
        assert_eq!(unknown.bytes, None);
    }

    #[test]
    fn artifact_size_estimate_rechaza_confianza_y_limites_inconsistentes() {
        let mut invalid_exact = ArtifactSizeEstimate {
            confidence: ArtifactSizeConfidence::Exact,
            bytes: crate::units::DecimalU64::new(10),
            lower_bound_bytes: crate::units::DecimalU64::new(9),
            upper_bound_bytes: crate::units::DecimalU64::new(10),
            source: ArtifactSizeSource::Manifest,
            evidence_refs: vec!["manifest:r1".into()],
            reason: None,
        };
        assert_eq!(
            invalid_exact.validate().unwrap_err().details.reason,
            "artifact_size_estimate_invalid"
        );
        invalid_exact.confidence = ArtifactSizeConfidence::Estimated;
        invalid_exact.lower_bound_bytes = None;
        invalid_exact.source = ArtifactSizeSource::Unknown;
        assert!(invalid_exact.validate().is_err());

        let mut empty_reason = ArtifactSizeEstimate::unknown("sin datos", "source:revision");
        empty_reason.reason = Some(String::new());
        assert!(empty_reason.validate().is_err());

        let mut no_evidence = ArtifactSizeEstimate::unknown("sin datos", "");
        no_evidence.evidence_refs.clear();
        assert!(no_evidence.validate().is_err());
    }

    #[test]
    fn roles_y_completitud() {
        let vision = vec!["text.generate".to_string(), "vision.understand".to_string()];
        assert_eq!(
            missing_roles(Some("GGUF"), &vision, &[artifact("weights", true)]),
            Some(vec![ArtifactRole::Mmproj]),
            "un GGUF de visión necesita su mmproj"
        );
        assert_eq!(
            missing_roles(
                Some("GGUF"),
                &vision,
                &[artifact("weights", true), artifact("mmproj", true)]
            ),
            Some(vec![])
        );
        assert_eq!(
            missing_roles(
                Some("SafeTensors"),
                &[],
                &[artifact("weights", true), artifact("tokenizer", false)]
            ),
            Some(vec![ArtifactRole::Tokenizer, ArtifactRole::Config]),
            "un opcional no cubre un rol necesario"
        );
        assert_eq!(
            missing_roles(Some("MLC_FUTURE"), &[], &[]),
            None,
            "formato desconocido: sin inventar"
        );
        assert_eq!(artifact("vae", true).role(), ArtifactRole::Vae);
        assert_eq!(artifact("raro", true).role(), ArtifactRole::Other);
        assert_eq!(
            artifact("raro", true)
                .validate()
                .unwrap_err()
                .details
                .reason,
            "role_invalid"
        );
        assert_eq!(
            serde_json::to_value(ArtifactRole::TextEncoder).unwrap(),
            "text_encoder"
        );
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    const ID: &str = "0192f000-0000-7000-8000-000000000001";
    const ID2: &str = "0192f000-0000-7000-8000-000000000002";
    const H: &str = "6b86b273ff34fce19d6b804eff5a3f5747ada4eaa22f1d49c01e52ddb7875b4b";

    #[test]
    fn capacidades_extensibles() {
        assert_eq!(
            capability("text.generate").unwrap(),
            Capability {
                id: "text.generate".into(),
                known: true
            }
        );
        assert_eq!(
            capability("ocr").unwrap().id,
            "vision.ocr",
            "alias de solo lectura"
        );
        let unknown = capability("audio.diarize").unwrap();
        assert!(!unknown.known, "se acepta pero no cuenta como soportada");
        for bad in [
            "",
            "Text.generate",
            "text..generate",
            ".x",
            "x.",
            "1x",
            "a b",
            "a-b",
        ] {
            assert_eq!(
                capability(bad).unwrap_err().details.reason,
                "capability_invalid",
                "{bad:?}"
            );
        }
        assert_eq!(
            capabilities(&[
                "vision".into(),
                "text.generate".into(),
                "vision.understand".into()
            ])
            .unwrap(),
            vec!["text.generate".to_string(), "vision.understand".to_string()]
        );
    }

    #[test]
    fn identificadores_de_formato() {
        for ok in [
            "GGUF",
            "Q4_K_M",
            "SafeTensors",
            "q4f16_1",
            "ONNX-INT4",
            "fp8.e4m3",
            "MLC_FUTURE",
        ] {
            assert!(identifier("format", ok).is_ok(), "{ok}");
        }
        assert_eq!(
            identifier("format", "").unwrap_err().details.reason,
            "format_invalid"
        );
        assert!(identifier("quantization", "q 4").is_err());
        assert!(identifier("quantization", &"x".repeat(65)).is_err());
    }

    #[test]
    fn claves_de_revision() {
        assert_eq!(
            revision_key("a1b2c3").unwrap(),
            RevisionKey::Upstream("a1b2c3".into())
        );
        assert_eq!(
            revision_key(&format!("pending:{ID}")).unwrap(),
            RevisionKey::Pending(ID.into())
        );
        assert_eq!(
            revision_key(&format!("content:{H}")).unwrap(),
            RevisionKey::Content(H.into())
        );
        for bad in ["", "pending:x", "content:ABC", "con espacio", "pending:"] {
            assert!(revision_key(bad).is_err(), "{bad:?}");
        }
    }

    fn revision(key: &str, state: RevisionState) -> ModelRevision {
        ModelRevision {
            id: ID.into(),
            created_at: String::new(),
            updated_at: String::new(),
            row_version: 1,
            metadata: Map::new(),
            distribution_id: ID2.into(),
            revision_key: key.into(),
            upstream_commit: None,
            manifest_hash: H.into(),
            state,
        }
    }

    #[test]
    fn reglas_de_revision() {
        let pending = format!("pending:{ID}");
        assert!(revision(&pending, RevisionState::Provisional)
            .validate()
            .is_ok());
        assert_eq!(
            revision(&pending, RevisionState::Current)
                .validate()
                .unwrap_err()
                .details
                .reason,
            "pending_revision_not_activatable"
        );
        assert_eq!(
            revision("abc123", RevisionState::Provisional)
                .validate()
                .unwrap_err()
                .details
                .reason,
            "provisional_requires_pending_key"
        );
        assert!(revision("abc123", RevisionState::Current)
            .validate()
            .is_ok());
        assert!(revision(&format!("content:{H}"), RevisionState::Current)
            .validate()
            .is_ok());
        let other = "a".repeat(64);
        assert_eq!(
            revision(&format!("content:{other}"), RevisionState::Current)
                .validate()
                .unwrap_err()
                .details
                .reason,
            "content_key_mismatch"
        );
    }

    fn model() -> CanonicalModel {
        CanonicalModel {
            id: ID.into(),
            created_at: "2026-10-11T00:00:00.000Z".into(),
            updated_at: "2026-10-11T00:00:00.000Z".into(),
            row_version: 1,
            metadata: Map::new(),
            name: "Qwen-X".into(),
            upstream_identity: Some("Qwen/Qwen-X".into()),
            capabilities: vec!["text.generate".into(), "tool.call".into()],
            possible_duplicates: vec![ID2.into()],
        }
    }

    #[test]
    fn modelo_canonico_y_roundtrip() {
        let m = model();
        assert!(m.validate().is_ok());
        let json = serde_json::to_value(&m).unwrap();
        assert_eq!(
            serde_json::from_value::<CanonicalModel>(json.clone()).unwrap(),
            m
        );
        let mut extra = json;
        extra["alias"] = Value::Bool(true);
        assert!(
            serde_json::from_value::<CanonicalModel>(extra).is_err(),
            "campos desconocidos"
        );
        let mut own = model();
        own.possible_duplicates = vec![ID.into()];
        assert!(own.validate().is_err(), "no es duplicado de sí mismo");
        let mut alias = model();
        alias.capabilities = vec!["ocr".into()];
        assert_eq!(
            alias.validate().unwrap_err().details.reason,
            "capabilities_not_normalized"
        );
    }

    fn provenance() -> ArtifactProvenance {
        let mut metadata = Map::new();
        metadata.insert("original_author".into(), "Qwen".into());
        metadata.insert("uploader".into(), "qwen".into());
        ArtifactProvenance {
            id: ID.into(),
            created_at: String::new(),
            updated_at: String::new(),
            row_version: 1,
            metadata,
            artifact_id: "0192f000-0000-7000-8000-0000000000a1".into(),
            source_id: Some(ID2.into()),
            revision_key: Some("abc".into()),
            parents: vec![],
            tool_id: None,
            tool_version: None,
            parameters: Map::new(),
            license_id: Some("apache-2.0".into()),
            license_hash: None,
            trust: Trust::Verified,
            actor_id: "importador".into(),
            evidence_refs: vec![],
        }
    }

    #[test]
    fn clase_de_origen() {
        let p = provenance();
        assert!(p.validate().is_ok());
        assert_eq!(origin_class(&p), OriginClass::OfficialPublisher);
        let mut other = p.clone();
        other.metadata.insert("uploader".into(), "bartowski".into());
        assert_eq!(origin_class(&other), OriginClass::VerifiedDistribution);
        let mut conv = p.clone();
        conv.parents = vec!["0192f000-0000-7000-8000-0000000000a9".into()];
        conv.tool_id = Some("llama.cpp/convert".into());
        conv.tool_version = Some("b4500".into());
        assert!(conv.validate().is_ok());
        assert_eq!(origin_class(&conv), OriginClass::CommunityConversion);
        let mut declared = p.clone();
        declared.trust = Trust::Declared;
        assert_eq!(origin_class(&declared), OriginClass::CommunityConversion);
        let mut none = p.clone();
        none.source_id = None;
        assert_eq!(origin_class(&none), OriginClass::UnknownOrigin);
        let mut revoked = p.clone();
        revoked.trust = Trust::Revoked;
        assert_eq!(origin_class(&revoked), OriginClass::UnknownOrigin);
        let mut tool = p;
        tool.tool_id = Some("x".into());
        assert_eq!(
            tool.validate().unwrap_err().details.reason,
            "tool_version_required"
        );
    }

    #[test]
    fn lifecycle_rechaza_transiciones_sin_evidencia_o_fuera_de_contrato() {
        let err = validate_lifecycle_transition(
            ModelLifecycle::Discovered,
            ModelLifecycle::Available,
            &LifecycleEvidence::default(),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidStateTransition);
        assert_eq!(err.details.reason, "invalid_state_transition");

        let evidence = LifecycleEvidence {
            selection_recorded: true,
            ..Default::default()
        };
        assert!(validate_lifecycle_transition(
            ModelLifecycle::Discovered,
            ModelLifecycle::Selected,
            &evidence,
        )
        .is_ok());
    }

    #[test]
    fn retry_requires_safe_state_and_new_attempt() {
        let mut evidence = LifecycleEvidence {
            last_safe_state: Some(ModelLifecycle::Downloaded),
            previous_attempt: 3,
            next_attempt: 4,
            ..Default::default()
        };
        assert!(validate_lifecycle_transition(
            ModelLifecycle::Failed,
            ModelLifecycle::Downloaded,
            &evidence,
        )
        .is_ok());
        evidence.next_attempt = 3;
        assert!(validate_lifecycle_transition(
            ModelLifecycle::Failed,
            ModelLifecycle::Downloaded,
            &evidence,
        )
        .is_err());
    }

    #[test]
    fn status_es_una_vista_y_update_coexiste_con_la_version_actual() {
        assert_eq!(
            ModelVariant::status_for("AVAILABLE", "AVAILABLE", true),
            "UPDATE_AVAILABLE"
        );
        assert_eq!(
            ModelVariant::status_for("AVAILABLE", "OFFLINE", true),
            "OFFLINE"
        );
        assert_eq!(
            ModelVariant::status_for("AVAILABLE", "CORRUPT", true),
            "CORRUPT"
        );
    }

    #[test]
    fn lifecycle_y_availability_serializan_los_valores_contractuales() {
        assert_eq!(
            serde_json::to_value(ModelLifecycle::TechnicallyValidated).unwrap(),
            "TECHNICALLY_VALIDATED"
        );
        assert_eq!(
            serde_json::from_value::<ModelLifecycle>(serde_json::json!("RUNNING")).unwrap(),
            ModelLifecycle::Running
        );
        assert_eq!(
            serde_json::to_value(ModelAvailability::Offline).unwrap(),
            "OFFLINE"
        );
        assert_eq!(
            serde_json::from_value::<ModelAvailability>(serde_json::json!("AVAILABLE")).unwrap(),
            ModelAvailability::Available
        );
    }
}
