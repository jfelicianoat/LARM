//! P026: binding y evidencia interna de una ejecución de conformidad.
//! No es un DTO de API: un JSON externo PASS no puede deserializar Outcome/Proof.
use crate::adapters::{AdapterInstallation, AdapterState, AdapterTrust};
use crate::error::{ErrorCode, LarmError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub const METADATA_KEY: &str = "larm_conformance_v1";
pub const JOURNAL_KIND: &str = "adapter.conformance";
pub const COMMON_CHECKS: [&str; 15] = [
    "handshake",
    "schema",
    "major",
    "request_id",
    "duplicate_reply",
    "cancel",
    "progress",
    "errors",
    "frame_limit",
    "malformed_json",
    "unannounced_method",
    "crash",
    "permissions",
    "state",
    "io_recovery",
];

pub fn invalid(reason: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "La evidencia de conformidad no es válida",
        reason,
    )
}
pub fn hash(value: &impl Serialize) -> Result<String, LarmError> {
    let value = serde_json::to_value(value).map_err(|_| invalid("conformance_binding_invalid"))?;
    crate::canonical::sha256_hex(&value).map_err(|_| invalid("conformance_binding_invalid"))
}
fn sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn capabilities(a: &AdapterInstallation) -> Result<Vec<String>, LarmError> {
    let raw = a
        .manifest
        .get("capabilities")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("conformance_manifest_invalid"))?;
    if raw.len() > 128 {
        return Err(invalid("conformance_manifest_invalid"));
    }
    let mut found = BTreeSet::new();
    for value in raw {
        let id = value
            .as_str()
            .ok_or_else(|| invalid("conformance_manifest_invalid"))?;
        let capability =
            crate::model::capability(id).map_err(|_| invalid("conformance_manifest_invalid"))?;
        if capability.id != id
            || (!capability.known && !id.contains('.'))
            || !found.insert(id.to_owned())
        {
            return Err(invalid("conformance_manifest_invalid"));
        }
    }
    Ok(found.into_iter().collect())
}
fn installation_hash(a: &AdapterInstallation) -> Result<String, LarmError> {
    let mut value = serde_json::to_value(a).map_err(|_| invalid("conformance_binding_invalid"))?;
    // Revisiones i64 exactas, también por encima de 2^53: nunca redondearlas.
    value["row_version"] = json!(a.row_version.to_string());
    hash(&value)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingHashes {
    pub suite_sha256: String,
    pub capability_profile_sha256: String,
    pub execution_context_sha256: String,
}
impl BindingHashes {
    fn validate(&self) -> Result<(), LarmError> {
        if [
            &self.suite_sha256,
            &self.capability_profile_sha256,
            &self.execution_context_sha256,
        ]
        .into_iter()
        .all(|s| sha(s))
        {
            Ok(())
        } else {
            Err(invalid("conformance_binding_invalid"))
        }
    }
}

/// Snapshot de datos registrados y del contexto que el ejecutor ha medido.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub id: String,
    pub actor_id: String,
    pub installation_id: String,
    pub installation_revision: String,
    pub installation_sha256: String,
    pub manifest_sha256: String,
    pub permissions_sha256: String,
    pub package_sha256: String,
    pub family: String,
    pub capabilities: Vec<String>,
    pub bindings: BindingHashes,
    pub started_at: String,
}
impl Run {
    pub fn new(
        a: &AdapterInstallation,
        bindings: BindingHashes,
        actor_id: &str,
    ) -> Result<Self, LarmError> {
        bindings.validate()?;
        if actor_id.is_empty() || actor_id.len() > 256 || actor_id.chars().any(char::is_control) {
            return Err(invalid("conformance_actor_invalid"));
        }
        if !crate::bridge::is_uuid_v7(&a.id)
            || a.row_version < 1
            || a.row_version == i64::MAX
            || !sha(&a.package_hash)
            || ![
                "runtime",
                "source",
                "protocol",
                "transformation",
                "benchmark",
            ]
            .contains(&a.family.as_str())
        {
            return Err(invalid("conformance_installation_invalid"));
        }
        if a.trust == AdapterTrust::Revoked {
            return Err(LarmError::new(
                ErrorCode::Forbidden,
                "El adaptador está revocado",
                "adapter_revoked",
            ));
        }
        if !matches!(a.state, AdapterState::Installed | AdapterState::Validated) {
            return Err(LarmError::new(
                ErrorCode::InvalidStateTransition,
                "La instalación no permite validación",
                "adapter_not_installed",
            ));
        }
        for (key, expected) in [
            ("id", &a.adapter_id),
            ("version", &a.version),
            ("family", &a.family),
            ("publisher", &a.publisher),
            ("package_sha256", &a.package_hash),
        ] {
            if a.manifest.get(key).and_then(Value::as_str) != Some(expected.as_str()) {
                return Err(invalid("conformance_manifest_mismatch"));
            }
        }
        let permissions = a
            .manifest
            .get("permissions")
            .filter(|p| p.is_object())
            .ok_or_else(|| invalid("conformance_manifest_invalid"))?;
        Ok(Self {
            id: uuid::Uuid::now_v7().to_string(),
            actor_id: actor_id.into(),
            installation_id: a.id.clone(),
            installation_revision: a.row_version.to_string(),
            installation_sha256: installation_hash(a)?,
            manifest_sha256: hash(&a.manifest)?,
            permissions_sha256: hash(permissions)?,
            package_sha256: a.package_hash.clone(),
            family: a.family.clone(),
            capabilities: capabilities(a)?,
            bindings,
            started_at: crate::bridge::timestamp(chrono::Utc::now()),
        })
    }
    pub fn digest(&self) -> Result<String, LarmError> {
        self.validate()?;
        hash(self)
    }
    pub fn validate(&self) -> Result<(), LarmError> {
        self.bindings.validate()?;
        let revision = self.installation_revision.parse::<i64>().ok();
        if !crate::bridge::is_uuid_v7(&self.id)
            || self.actor_id.is_empty()
            || self.actor_id.len() > 256
            || self.actor_id.chars().any(char::is_control)
            || !crate::bridge::is_uuid_v7(&self.installation_id)
            || !revision.is_some_and(|r| {
                r > 0 && r < i64::MAX && r.to_string() == self.installation_revision
            })
            || ![
                &self.installation_sha256,
                &self.manifest_sha256,
                &self.permissions_sha256,
                &self.package_sha256,
            ]
            .into_iter()
            .all(|s| sha(s))
            || ![
                "runtime",
                "source",
                "protocol",
                "transformation",
                "benchmark",
            ]
            .contains(&self.family.as_str())
            || chrono::DateTime::parse_from_rfc3339(&self.started_at).is_err()
            || !self.started_at.ends_with('Z')
            || self.capabilities.len() > 128
        {
            return Err(invalid("conformance_binding_invalid"));
        }
        let mut ids = BTreeSet::new();
        for id in &self.capabilities {
            let parsed =
                crate::model::capability(id).map_err(|_| invalid("conformance_binding_invalid"))?;
            if parsed.id != *id || (!parsed.known && !id.contains('.')) || !ids.insert(id.clone()) {
                return Err(invalid("conformance_binding_invalid"));
            }
        }
        if ids.into_iter().collect::<Vec<_>>() != self.capabilities {
            return Err(invalid("conformance_binding_invalid"));
        }
        Ok(())
    }
    pub fn matches(&self, a: &AdapterInstallation) -> Result<bool, LarmError> {
        self.validate()?;
        Ok(a.id == self.installation_id
            && a.row_version.to_string() == self.installation_revision
            && installation_hash(a)? == self.installation_sha256
            && a.package_hash == self.package_sha256
            && a.family == self.family
            && capabilities(a)? == self.capabilities
            && hash(&a.manifest)? == self.manifest_sha256
            && a.manifest
                .get("permissions")
                .map(hash)
                .transpose()?
                .as_ref()
                == Some(&self.permissions_sha256))
    }
    pub fn required_checks(&self) -> BTreeSet<String> {
        COMMON_CHECKS
            .into_iter()
            .map(String::from)
            .chain(std::iter::once(format!("family:{}", self.family)))
            .chain(
                self.capabilities
                    .iter()
                    .map(|id| format!("capability:{id}")),
            )
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Observation {
    pub check_id: String,
    pub evidence_sha256: String,
    pub passed: bool,
}

/// Se construye desde observaciones del ejecutor, nunca desde JSON de usuario.
/// ```compile_fail
/// let _: larm_domain::conformance::Proof = serde_json::from_str("{}").unwrap();
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Proof {
    run_sha256: String,
    observations: Vec<Observation>,
}
impl Proof {
    pub fn evaluate(run: &Run, mut observations: Vec<Observation>) -> Result<Self, LarmError> {
        let run_sha256 = run.digest()?;
        let mut ids = BTreeSet::new();
        for observation in &observations {
            if !observation.passed {
                return Err(invalid("conformance_check_failed"));
            }
            if !sha(&observation.evidence_sha256) || !ids.insert(observation.check_id.clone()) {
                return Err(invalid("conformance_evidence_invalid"));
            }
        }
        if ids != run.required_checks() {
            return Err(invalid("conformance_coverage_incomplete"));
        }
        observations.sort_by(|a, b| a.check_id.cmp(&b.check_id));
        Ok(Self {
            run_sha256,
            observations,
        })
    }
    pub fn check_run(&self, run: &Run) -> Result<(), LarmError> {
        if self.run_sha256 != run.digest()? {
            return Err(invalid("conformance_run_mismatch"));
        }
        Self::evaluate(run, self.observations.clone()).map(|_| ())
    }
    pub fn digest(&self) -> Result<String, LarmError> {
        hash(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Outcome {
    Passed { proof: Proof },
    Failed { error_code: String },
}
impl Outcome {
    pub fn failed(code: &ErrorCode) -> Self {
        // Other(String) puede proceder del plugin; nunca persistir ese texto.
        let safe = match code {
            ErrorCode::Other(_) => ErrorCode::Internal,
            _ => code.clone(),
        };
        Self::Failed {
            error_code: safe.as_str().into(),
        }
    }
    pub fn check_run(&self, run: &Run) -> Result<(), LarmError> {
        match self {
            Self::Passed { proof } => proof.check_run(run),
            Self::Failed { error_code } => {
                let code = ErrorCode::from(error_code.as_str());
                if matches!(code, ErrorCode::Other(_)) {
                    Err(invalid("conformance_error_code_invalid"))
                } else {
                    run.validate()
                }
            }
        }
    }
    pub fn passed(&self) -> bool {
        matches!(self, Self::Passed { .. })
    }
    pub fn evidence_hash(&self) -> Result<Option<String>, LarmError> {
        match self {
            Self::Passed { proof } => proof.digest().map(Some),
            Self::Failed { .. } => Ok(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub run_id: String,
    pub installation_id: String,
    pub installation_revision: String,
    pub committed_installation_sha256: String,
    pub package_sha256: String,
    pub run_sha256: String,
    pub outcome_sha256: String,
    pub evidence_sha256: Option<String>,
    pub passed: bool,
    pub completed_at: String,
}

/// Hash del estado confirmado, excluyendo sólo el propio recibo para evitar
/// una dependencia circular. También cubre confianza, metadata y timestamps.
pub fn committed_installation_hash(a: &AdapterInstallation) -> Result<String, LarmError> {
    let mut snapshot = a.clone();
    snapshot.metadata.remove(METADATA_KEY);
    installation_hash(&snapshot)
}
impl Receipt {
    pub fn matches_committed(&self, a: &AdapterInstallation) -> Result<bool, LarmError> {
        Ok(self.passed
            && a.state == AdapterState::Validated
            && a.trust != AdapterTrust::Revoked
            && self.installation_id == a.id
            && self.installation_revision == a.row_version.to_string()
            && self.package_sha256 == a.package_hash
            && sha(&self.committed_installation_sha256)
            && self.committed_installation_sha256 == committed_installation_hash(a)?)
    }
}

/// Resultado interno del ejecutor. Error antes de construirlo (p.ej. permiso
/// denegado) no es un fallo de conformidad y no invalida una instalación.
/// El ejecutor sólo devuelve esto después de cerrar/esperar sus procesos e IO.
pub struct Execution {
    pub run: Run,
    pub proof: Result<Proof, LarmError>,
}
