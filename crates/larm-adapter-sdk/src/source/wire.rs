//! Tipos de las firmas source obligatorias y del método opcional de bloques.
use base64::{engine::general_purpose::STANDARD, Engine};
pub use larm_domain::acquisition::SourceCatalogReference;
use larm_domain::adapter_updates::is_sha256;
use larm_domain::artifacts::Plan;
use larm_domain::bridge::{is_uuid_v7, JobReceipt, Page};
pub use larm_domain::download::DownloadPriority;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::estimate::StorageEstimate;
use larm_domain::model::{bundle_path, ModelRevision, ModelVariant};
use larm_domain::units::DecimalU64;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeSet;

pub const SOURCE_DOWNLOAD_CHUNK_MAX_BYTES: u64 = 524_288;
const SOURCE_DOWNLOAD_CHUNK_MAX_BASE64: usize = 699_052;

use super::{invalid, RateLimit, SourceSearch};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamRef {
    pub source_id: String,
    pub upstream_namespace: String,
    pub upstream_model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionRef {
    pub source_id: String,
    pub upstream_namespace: String,
    pub upstream_model_id: String,
    pub revision_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadPlanRequest {
    pub variant_id: String,
    pub storage_location_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub hard_budget_bytes: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub source_id: Option<String>,
    pub priority: DownloadPriority,
    /// Core-owned artifact IDs selected by the operator. `None` preserves an
    /// omitted field; `Some([])` preserves an explicit empty selection.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_optional_artifacts"
    )]
    pub optional_artifact_ids: Option<Vec<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_acquisition"
    )]
    pub acquisition: Option<super::SourceAcquisitionBinding>,
}

fn present_acquisition<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<super::SourceAcquisitionBinding>, D::Error> {
    super::SourceAcquisitionBinding::deserialize(d).map(Some)
}

fn present_optional_artifacts<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<String>>, D::Error> {
    Vec::<String>::deserialize(d).map(Some)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutePlan {
    pub plan_id: String,
    pub plan_hash: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub approval_id: Option<String>,
}

/// Validador HTTP fijado para una lectura reanudable. Un cambio entre
/// respuesta y checkpoint obliga al executor a volver al offset cero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum SourceDownloadResumeValidator {
    #[serde(rename = "STRONG_ETAG")]
    StrongEtag { value: String },
    #[serde(rename = "LAST_MODIFIED_AND_SHA256")]
    LastModifiedAndSha256 {
        last_modified: String,
        sha256: String,
    },
}

impl SourceDownloadResumeValidator {
    pub fn validate(&self) -> Result<(), LarmError> {
        match self {
            Self::StrongEtag { value } => {
                let bytes = value.as_bytes();
                if bytes.len() < 2
                    || bytes.first() != Some(&b'"')
                    || bytes.last() != Some(&b'"')
                    || bytes[1..bytes.len() - 1]
                        .iter()
                        .any(|byte| *byte == b'"' || !(0x21..=0x7e).contains(byte))
                {
                    return Err(invalid("strong_etag"));
                }
                Ok(())
            }
            Self::LastModifiedAndSha256 {
                last_modified,
                sha256,
            } => {
                if last_modified.is_empty()
                    || last_modified.len() > 1024
                    || last_modified.chars().any(char::is_control)
                    || !is_sha256(sha256)
                {
                    return Err(invalid("last_modified_sha256"));
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDownloadChunkRequest {
    pub source_id: String,
    pub job_id: String,
    pub plan_id: String,
    pub plan_hash: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub approval_id: Option<String>,
    pub revision: RevisionRef,
    pub relative_path: String,
    pub offset: DecimalU64,
    pub max_bytes: DecimalU64,
    #[serde(deserialize_with = "Option::deserialize")]
    pub validator: Option<SourceDownloadResumeValidator>,
}

impl SourceDownloadChunkRequest {
    pub fn validate(&self) -> Result<(), LarmError> {
        uuid(&self.source_id)?;
        uuid(&self.job_id)?;
        uuid(&self.plan_id)?;
        if !is_sha256(&self.plan_hash) {
            return Err(invalid("plan_hash"));
        }
        if let Some(approval_id) = &self.approval_id {
            uuid(approval_id)?;
        }
        self.revision.validate()?;
        if self.revision.source_id != self.source_id {
            return Err(invalid("source_identity_mismatch"));
        }
        if self.relative_path.len() > 4096 {
            return Err(invalid("relative_path"));
        }
        bundle_path(&self.relative_path)?;
        if !(1..=SOURCE_DOWNLOAD_CHUNK_MAX_BYTES).contains(&self.max_bytes.get()) {
            return Err(invalid("chunk_max_bytes"));
        }
        if let Some(validator) = &self.validator {
            validator.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDownloadChunk {
    pub offset: DecimalU64,
    pub data_base64: String,
    pub eof: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    pub total_bytes: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub validator: Option<SourceDownloadResumeValidator>,
}

impl SourceDownloadChunk {
    /// Decodifica sólo base64 canónico; no acepta alias, whitespace ni padding
    /// omitido y limita la asignación al máximo wire publicado.
    pub fn decoded_bytes(&self) -> Result<Vec<u8>, LarmError> {
        if self.data_base64.len() > SOURCE_DOWNLOAD_CHUNK_MAX_BASE64 {
            return Err(invalid("chunk_encoded_size"));
        }
        let bytes = STANDARD
            .decode(&self.data_base64)
            .map_err(|_| invalid("chunk_base64"))?;
        if STANDARD.encode(&bytes) != self.data_base64 {
            return Err(invalid("chunk_base64_noncanonical"));
        }
        Ok(bytes)
    }

    fn validate_for(&self, request: &SourceDownloadChunkRequest) -> Result<(), LarmError> {
        request.validate()?;
        if self.offset != request.offset {
            return Err(invalid("chunk_offset_mismatch"));
        }
        let bytes = self.decoded_bytes()?;
        if bytes.len() as u64 > request.max_bytes.get() || (bytes.is_empty() && !self.eof) {
            return Err(invalid("chunk_size_or_progress"));
        }
        let end = self
            .offset
            .get()
            .checked_add(bytes.len() as u64)
            .filter(|value| *value <= larm_domain::units::DECIMAL_U64_MAX)
            .ok_or_else(|| invalid("chunk_offset_overflow"))?;
        match self.total_bytes {
            Some(total) if self.eof && total.get() == end => {}
            Some(total) if !self.eof && total.get() > end => {}
            None if !self.eof => {}
            _ => return Err(invalid("chunk_eof_total_mismatch")),
        }
        if let Some(validator) = &self.validator {
            validator.validate()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncRequest {
    pub source_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub cursor: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub etag: Option<String>,
    pub query: Map<String, Value>,
}

pub use larm_domain::sources::AuthMode;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthProbe {
    pub source_id: String,
    pub auth_mode: AuthMode,
    #[serde(deserialize_with = "Option::deserialize")]
    pub account_id: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub credential_reference_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthResult {
    pub authenticated: bool,
    pub auth_required: bool,
    pub license_required: bool,
    pub reason: String,
}

/// Descriptor de ModelInspection/ArtifactManifest. Unknown sigue siendo null.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceArtifact {
    pub relative_path: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub size_bytes: Option<DecimalU64>,
    /// Estimación con procedencia y límites. Ausente conserva la semántica
    /// v3 anterior de `size_bytes`; UNKNOWN no se convierte en cero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_estimate: Option<larm_domain::model::ArtifactSizeEstimate>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub sha256: Option<String>,
    pub required: bool,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelInspection {
    #[serde(deserialize_with = "Option::deserialize")]
    pub upstream_identity: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub architecture: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub format: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub quantization: Option<String>,
    pub capabilities: Vec<String>,
    pub limits: Map<String, Value>,
    pub artifacts: Vec<SourceArtifact>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub license: Option<String>,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactManifest {
    pub revision_key: String,
    pub manifest_hash: String,
    pub artifacts: Vec<SourceArtifact>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncResult {
    #[serde(deserialize_with = "Option::deserialize")]
    pub cursor: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub etag: Option<String>,
    pub changed: bool,
    pub models: Vec<ModelInspection>,
    /// Límite observado en la respuesta del proveedor. Ausente conserva
    /// compatibilidad con adaptadores fuente anteriores.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_rate_limit"
    )]
    pub rate_limit: Option<Option<RateLimit>>,
}

fn optional_rate_limit<'de, D>(deserializer: D) -> Result<Option<Option<RateLimit>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::deserialize(deserializer).map(Some)
}

pub type ModelPage = Page<ModelVariant>;
pub type RevisionPage = Page<ModelRevision>;

/// Unión interna; el wire sigue siendo method + params, sin un tag adicional.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceRequest {
    Search(SourceSearch),
    GetModel(UpstreamRef),
    ListRevisions(UpstreamRef),
    GetManifest(RevisionRef),
    EstimateSize(RevisionRef),
    DownloadPlan(DownloadPlanRequest),
    Download(ExecutePlan),
    DownloadChunk(SourceDownloadChunkRequest),
    Sync(SyncRequest),
    AuthProbe(AuthProbe),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceReply {
    Search(ModelPage),
    GetModel(ModelInspection),
    ListRevisions(RevisionPage),
    GetManifest(ArtifactManifest),
    EstimateSize(StorageEstimate),
    DownloadPlan(Plan),
    Download(JobReceipt),
    DownloadChunk(SourceDownloadChunk),
    Sync(SyncResult),
    AuthProbe(AuthResult),
}

fn uuid(value: &str) -> Result<(), LarmError> {
    if is_uuid_v7(value) {
        Ok(())
    } else {
        Err(invalid("id"))
    }
}
fn text(value: &str) -> Result<(), LarmError> {
    if value.is_empty() {
        Err(invalid("text"))
    } else {
        Ok(())
    }
}
fn nullable_text(value: &Option<String>) -> Result<(), LarmError> {
    if value.as_deref() == Some("") {
        Err(invalid("nullable_text"))
    } else {
        Ok(())
    }
}
pub(crate) fn time(value: &str) -> Result<(), LarmError> {
    let valid = chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .is_some_and(|v| larm_domain::bridge::timestamp(v.with_timezone(&chrono::Utc)) == value);
    if valid {
        Ok(())
    } else {
        Err(invalid("timestamp"))
    }
}

/// Comparar roundtrip evita que Option convierta una clave required ausente
/// de tipos compartidos en null. No se eliminan campos de metadata abiertos.
pub(crate) fn decode<T: DeserializeOwned + Serialize>(value: Value) -> Result<T, LarmError> {
    let result: T = serde_json::from_value(value.clone()).map_err(|_| invalid("schema"))?;
    if serde_json::to_value(&result).map_err(|_| invalid("serialize"))? != value {
        return Err(invalid("required_fields"));
    }
    Ok(result)
}
fn encode<T: Serialize>(value: &T) -> Result<Value, LarmError> {
    serde_json::to_value(value).map_err(|_| invalid("serialize"))
}

impl UpstreamRef {
    pub fn validate(&self) -> Result<(), LarmError> {
        uuid(&self.source_id)?;
        text(&self.upstream_namespace)?;
        text(&self.upstream_model_id)
    }
}
impl RevisionRef {
    pub fn validate(&self) -> Result<(), LarmError> {
        UpstreamRef {
            source_id: self.source_id.clone(),
            upstream_namespace: self.upstream_namespace.clone(),
            upstream_model_id: self.upstream_model_id.clone(),
        }
        .validate()?;
        larm_domain::model::revision_key(&self.revision_key)?;
        Ok(())
    }
}
pub fn validate_artifacts(files: &[SourceArtifact]) -> Result<(), LarmError> {
    let mut seen = BTreeSet::new();
    for file in files {
        bundle_path(&file.relative_path)?;
        text(&file.role)?;
        if !seen.insert(file.relative_path.to_lowercase()) {
            return Err(invalid("duplicate_artifact"));
        }
        if file.sha256.as_deref().is_some_and(|h| !is_sha256(h)) {
            return Err(invalid("sha256"));
        }
        if let Some(estimate) = &file.size_estimate {
            estimate.validate()?;
            if file.size_bytes.is_some_and(|size| {
                estimate.confidence != larm_domain::model::ArtifactSizeConfidence::Exact
                    || estimate.bytes != Some(size)
            }) {
                return Err(invalid("size_estimate_conflicts_with_manifest"));
            }
        }
    }
    Ok(())
}
impl ModelInspection {
    pub fn validate(&self) -> Result<(), LarmError> {
        for field in [
            &self.upstream_identity,
            &self.architecture,
            &self.format,
            &self.quantization,
            &self.license,
        ] {
            nullable_text(field)?;
        }
        // La revisión 3.1.0 añade `size_estimate` a ArtifactManifest, no a
        // ModelInspection. No permitir que el tipo compartido amplíe su schema.
        if self
            .artifacts
            .iter()
            .any(|artifact| artifact.size_estimate.is_some())
        {
            return Err(invalid("inspection_size_estimate_not_allowed"));
        }
        validate_artifacts(&self.artifacts)
    }
}
impl ArtifactManifest {
    pub fn validate(&self) -> Result<(), LarmError> {
        larm_domain::model::revision_key(&self.revision_key)?;
        if !is_sha256(&self.manifest_hash) {
            return Err(invalid("manifest_hash"));
        }
        validate_artifacts(&self.artifacts)
    }
}

impl SourceRequest {
    pub fn method(&self) -> &'static str {
        match self {
            Self::Search(_) => "source.search",
            Self::GetModel(_) => "source.get_model",
            Self::ListRevisions(_) => "source.list_revisions",
            Self::GetManifest(_) => "source.get_manifest",
            Self::EstimateSize(_) => "source.estimate_size",
            Self::DownloadPlan(_) => "source.download_plan",
            Self::Download(_) => "source.download",
            Self::DownloadChunk(_) => "source.download_chunk",
            Self::Sync(_) => "source.sync",
            Self::AuthProbe(_) => "source.auth_probe",
        }
    }
    pub fn source_id(&self) -> Option<&str> {
        match self {
            Self::Search(v) => Some(&v.source_id),
            Self::GetModel(v) | Self::ListRevisions(v) => Some(&v.source_id),
            Self::GetManifest(v) | Self::EstimateSize(v) => Some(&v.source_id),
            Self::DownloadPlan(v) => v.source_id.as_deref(),
            Self::Download(_) => None,
            Self::DownloadChunk(v) => Some(&v.source_id),
            Self::Sync(v) => Some(&v.source_id),
            Self::AuthProbe(v) => Some(&v.source_id),
        }
    }
    pub fn parse(method: &str, value: Value) -> Result<Self, LarmError> {
        let request = match method {
            "source.search" => Self::Search(decode(value)?),
            "source.get_model" => Self::GetModel(decode(value)?),
            "source.list_revisions" => Self::ListRevisions(decode(value)?),
            "source.get_manifest" => Self::GetManifest(decode(value)?),
            "source.estimate_size" => Self::EstimateSize(decode(value)?),
            "source.download_plan" => Self::DownloadPlan(decode(value)?),
            "source.download" => Self::Download(decode(value)?),
            "source.download_chunk" => Self::DownloadChunk(decode(value)?),
            "source.sync" => Self::Sync(decode(value)?),
            "source.auth_probe" => Self::AuthProbe(decode(value)?),
            _ => {
                return Err(LarmError::new(
                    ErrorCode::UnsupportedCapability,
                    "Método source no publicado",
                    "source_method_not_supported",
                ))
            }
        };
        request.validate()?;
        Ok(request)
    }
    pub fn params(&self) -> Result<Value, LarmError> {
        self.validate()?;
        match self {
            Self::Search(v) => encode(v),
            Self::GetModel(v) | Self::ListRevisions(v) => encode(v),
            Self::GetManifest(v) | Self::EstimateSize(v) => encode(v),
            Self::DownloadPlan(v) => encode(v),
            Self::Download(v) => encode(v),
            Self::DownloadChunk(v) => encode(v),
            Self::Sync(v) => encode(v),
            Self::AuthProbe(v) => encode(v),
        }
    }
    pub fn validate(&self) -> Result<(), LarmError> {
        match self {
            Self::Search(v) => v.validate(),
            Self::GetModel(v) | Self::ListRevisions(v) => v.validate(),
            Self::GetManifest(v) | Self::EstimateSize(v) => v.validate(),
            Self::DownloadPlan(v) => {
                uuid(&v.variant_id)?;
                uuid(&v.storage_location_id)?;
                if let Some(source) = &v.source_id {
                    uuid(source)?;
                }
                if let Some(ids) = &v.optional_artifact_ids {
                    if ids.len() > 4096 {
                        return Err(invalid("optional_artifact_ids"));
                    }
                    let mut seen = BTreeSet::new();
                    for id in ids {
                        if !is_uuid_v7(id) || !seen.insert(id) {
                            return Err(invalid("optional_artifact_ids"));
                        }
                    }
                }
                if let Some(binding) = &v.acquisition {
                    binding.validate_for(v)?;
                }
                Ok(())
            }
            Self::Download(v) => {
                uuid(&v.plan_id)?;
                if !is_sha256(&v.plan_hash) {
                    return Err(invalid("plan_hash"));
                }
                if let Some(approval) = &v.approval_id {
                    uuid(approval)?;
                }
                Ok(())
            }
            Self::DownloadChunk(v) => v.validate(),
            Self::Sync(v) => {
                uuid(&v.source_id)?;
                nullable_text(&v.cursor)?;
                nullable_text(&v.etag)
            }
            Self::AuthProbe(v) => {
                uuid(&v.source_id)?;
                if let Some(account) = &v.account_id {
                    uuid(account)?;
                }
                if let Some(credential) = &v.credential_reference_id {
                    uuid(credential)?;
                }
                match (&v.auth_mode, &v.account_id, &v.credential_reference_id) {
                    (AuthMode::None, None, None)
                    | (AuthMode::WindowsIntegrated, None, None)
                    | (AuthMode::WindowsIntegrated, Some(_), Some(_))
                    | (_, Some(_), Some(_)) => {}
                    _ => {
                        return Err(LarmError::new(
                            ErrorCode::ValidationFailed,
                            "Vínculo de credencial del sondeo no válido",
                            "auth_probe_credential_binding",
                        ));
                    }
                }
                Ok(())
            }
        }
    }
}

impl SourceReply {
    pub fn parse(request: &SourceRequest, value: Value) -> Result<Self, LarmError> {
        let result = (|| {
            let reply = match request {
                SourceRequest::Search(_) => Self::Search(decode(value)?),
                SourceRequest::GetModel(_) => Self::GetModel(decode(value)?),
                SourceRequest::ListRevisions(_) => Self::ListRevisions(decode(value)?),
                SourceRequest::GetManifest(_) => Self::GetManifest(decode(value)?),
                SourceRequest::EstimateSize(_) => Self::EstimateSize(decode(value)?),
                SourceRequest::DownloadPlan(_) => Self::DownloadPlan(decode(value)?),
                SourceRequest::Download(_) => Self::Download(decode(value)?),
                SourceRequest::DownloadChunk(_) => Self::DownloadChunk(decode(value)?),
                SourceRequest::Sync(_) => Self::Sync(decode(value)?),
                SourceRequest::AuthProbe(_) => Self::AuthProbe(decode(value)?),
            };
            reply.validate_for(request)?;
            Ok(reply)
        })();
        result.map_err(|_: LarmError| {
            LarmError::new(
                ErrorCode::ProtocolError,
                "Respuesta source fuera de contrato",
                "source_response_invalid",
            )
        })
    }
    pub fn value(&self) -> Result<Value, LarmError> {
        match self {
            Self::Search(v) => encode(v),
            Self::GetModel(v) => encode(v),
            Self::ListRevisions(v) => encode(v),
            Self::GetManifest(v) => encode(v),
            Self::EstimateSize(v) => encode(v),
            Self::DownloadPlan(v) => encode(v),
            Self::Download(v) => encode(v),
            Self::DownloadChunk(v) => encode(v),
            Self::Sync(v) => encode(v),
            Self::AuthProbe(v) => encode(v),
        }
    }
    pub fn validate_for(&self, request: &SourceRequest) -> Result<(), LarmError> {
        match (request, self) {
            (SourceRequest::Search(q), Self::Search(p)) => {
                page(p, Some(q.limit))?;
                let mut ids = BTreeSet::new();
                for item in &p.items {
                    uuid(&item.id)?;
                    uuid(&item.revision_id)?;
                    time(&item.created_at)?;
                    time(&item.updated_at)?;
                    if item.row_version < 1 || !ids.insert(&item.id) {
                        return Err(invalid("model_page"));
                    }
                    item.validate()?;
                    if larm_domain::acquisition::catalog_reference(item)?
                        .is_some_and(|reference| reference.source_id != q.source_id)
                    {
                        return Err(invalid("source_catalog_reference_mismatch"));
                    }
                }
                Ok(())
            }
            (SourceRequest::GetModel(_), Self::GetModel(v)) => v.validate(),
            (SourceRequest::ListRevisions(_), Self::ListRevisions(p)) => {
                page(p, None)?;
                let mut ids = BTreeSet::new();
                for item in &p.items {
                    uuid(&item.id)?;
                    uuid(&item.distribution_id)?;
                    time(&item.created_at)?;
                    time(&item.updated_at)?;
                    if item.row_version < 1 || !ids.insert(&item.id) {
                        return Err(invalid("revision_page"));
                    }
                    item.validate()?;
                }
                Ok(())
            }
            (SourceRequest::GetManifest(q), Self::GetManifest(v)) => {
                v.validate()?;
                if v.revision_key != q.revision_key {
                    return Err(invalid("revision_key_mismatch"));
                }
                Ok(())
            }
            (SourceRequest::EstimateSize(_), Self::EstimateSize(v)) => {
                text(&v.reason)?;
                for volume in &v.per_volume {
                    text(&volume.volume_key)?;
                    text(&volume.reason)?;
                }
                Ok(())
            }
            (SourceRequest::DownloadPlan(q), Self::DownloadPlan(v)) => {
                uuid(&v.plan_id)?;
                time(&v.expires_at)?;
                text(&v.operation)?;
                if !is_sha256(&v.plan_hash) {
                    return Err(invalid("plan_hash"));
                }
                for step in &v.steps {
                    text(&step.phase)?;
                    nullable_text(&step.adapter_id)?;
                }
                super::validate_source_plan(q, v)
            }
            (SourceRequest::Download(_), Self::Download(v)) => {
                uuid(&v.job_id)?;
                // JobReceipt es compartido por API y workers. El schema exige
                // una URL no vacía, no que el worker posea un job del Core.
                text(&v.status_url)
            }
            (SourceRequest::DownloadChunk(request), Self::DownloadChunk(chunk)) => {
                chunk.validate_for(request)
            }
            (SourceRequest::Sync(_), Self::Sync(v)) => {
                nullable_text(&v.cursor)?;
                nullable_text(&v.etag)?;
                if let Some(rate_limit) = v.rate_limit.as_ref().and_then(Option::as_ref) {
                    rate_limit.validate()?;
                }
                for item in &v.models {
                    item.validate()?;
                }
                Ok(())
            }
            (SourceRequest::AuthProbe(_), Self::AuthProbe(v)) => text(&v.reason),
            _ => Err(LarmError::new(
                ErrorCode::ProtocolError,
                "Tipo de respuesta no corresponde al método",
                "source_response_mismatch",
            )),
        }
    }
}

fn page<T>(p: &Page<T>, limit: Option<u16>) -> Result<(), LarmError> {
    time(&p.snapshot_at)?;
    nullable_text(&p.next_cursor)?;
    if limit.is_some_and(|n| p.items.len() > usize::from(n)) {
        return Err(invalid("page_limit"));
    }
    Ok(())
}
