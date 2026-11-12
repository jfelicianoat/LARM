//! P043: frontera SourceSearch (schema del SDK) frente a Search completo H05.
//! El adaptador no anuncia como upstream filtros que sólo puede aplicar Core.
//! La búsqueda federada, sus cursores firmados y el postfiltrado son de P044.

use std::collections::BTreeSet;

use larm_domain::bridge::is_uuid_v7;
use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::search::{CompatibilityFilter, Search};
use serde::{Deserialize, Serialize};

mod wire;
pub use wire::*;
mod acquisition;
pub use acquisition::*;
mod metadata;
pub use metadata::*;
mod pagination;
pub use pagination::*;
mod download;
pub use download::*;
mod handler;
pub use handler::*;

pub const TEXT_FILTER: &str = "source.search.q";
pub const CAPABILITY_FILTER: &str = "source.search.capabilities";

fn invalid(field: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        "Documento de fuente no válido",
        "source_search_invalid",
    )
    .with_resource(field.to_string())
}

/// DTO exacto de `source.search`: no recibe el cursor federado ni filtros
/// adicionales de Search. Un cursor null empieza; su clave sigue siendo obligatoria.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSearch {
    pub source_id: String,
    pub q: String,
    pub capabilities: Vec<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub cursor: Option<String>,
    pub limit: u16,
}

impl SourceSearch {
    pub fn validate(&self) -> Result<(), LarmError> {
        if !is_uuid_v7(&self.source_id) {
            return Err(invalid("source_id"));
        }
        if self.q.chars().count() > 500 {
            return Err(invalid("q"));
        }
        if self.cursor.as_deref() == Some("") {
            return Err(invalid("cursor"));
        }
        if !(1..=200).contains(&self.limit) {
            return Err(invalid("limit"));
        }
        Ok(())
    }
}

/// Declaración local de negociación, destinada a metadata del catálogo.
/// No añade campos a AdapterManifest, SourceSearch ni ModelPage.
/// Identificadores de vendor desconocidos se conservan sin interpretarlos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchSupport {
    pub search_applicable: bool,
    pub supported_filters: BTreeSet<String>,
    pub limitations: Vec<String>,
}

impl SearchSupport {
    pub fn validate(&self) -> Result<(), LarmError> {
        for name in &self.supported_filters {
            if !crate::manifest::is_namespaced(name)
                || name.starts_with("core.")
                || (name.starts_with("source.") && name != TEXT_FILTER && name != CAPABILITY_FILTER)
            {
                return Err(invalid("supported_filters"));
            }
        }
        if !self.search_applicable && !self.supported_filters.is_empty() {
            return Err(invalid("search_applicable"));
        }
        Ok(())
    }

    /// Comprobar antes de consultar upstream. Un adaptador import-only no
    /// responde con una página vacía que finja soportar búsqueda.
    pub fn require(&self, request: &SourceSearch) -> Result<(), LarmError> {
        self.validate()?;
        request.validate()?;
        let unsupported = !self.search_applicable
            || (!request.q.is_empty() && !self.supported_filters.contains(TEXT_FILTER))
            || (!request.capabilities.is_empty()
                && !self.supported_filters.contains(CAPABILITY_FILTER));
        if unsupported {
            return Err(LarmError::new(
                ErrorCode::UnsupportedCapability,
                "La fuente no admite esta búsqueda o filtro upstream",
                "source_filter_not_supported",
            ));
        }
        Ok(())
    }

    /// Preparar la petición upstream y el trabajo que conserva Core. El cursor
    /// upstream se recibe aparte: nunca se transmite Search.cursor al plugin.
    pub fn project(
        &self,
        full: &Search,
        source_id: &str,
        upstream_cursor: Option<String>,
    ) -> Result<SearchProjection, LarmError> {
        self.validate()?;
        full.validate()?;
        let mut deferred = BTreeSet::new();
        let q = if self.supported_filters.contains(TEXT_FILTER) {
            full.q.clone()
        } else {
            if !full.q.is_empty() {
                deferred.insert("q");
            }
            String::new()
        };
        let capabilities = if self.supported_filters.contains(CAPABILITY_FILTER) {
            full.capabilities.clone()
        } else {
            if !full.capabilities.is_empty() {
                deferred.insert("capabilities");
            }
            Vec::new()
        };
        for (field, active) in [
            ("source_ids", !full.source_ids.is_empty()),
            ("architectures", !full.architectures.is_empty()),
            ("formats", !full.formats.is_empty()),
            ("quantizations", !full.quantizations.is_empty()),
            ("runtime_ids", !full.runtime_ids.is_empty()),
            ("licenses", !full.licenses.is_empty()),
            (
                "parameter_count",
                full.parameter_count.minimum.is_some() || full.parameter_count.maximum.is_some(),
            ),
            (
                "size_bytes",
                full.size_bytes.minimum.is_some() || full.size_bytes.maximum.is_some(),
            ),
            (
                "trust_score",
                full.trust_score.minimum.is_some() || full.trust_score.maximum.is_some(),
            ),
            (
                "certification_statuses",
                !full.certification_statuses.is_empty(),
            ),
            (
                "compatibility",
                full.compatibility != CompatibilityFilter::All,
            ),
            ("only_executable", full.only_executable),
            ("only_recommended", full.only_recommended),
            ("show_incompatible", !full.show_incompatible),
        ] {
            if active {
                deferred.insert(field);
            }
        }
        let upstream = SourceSearch {
            source_id: source_id.to_string(),
            q,
            capabilities,
            cursor: upstream_cursor,
            limit: full.limit,
        };
        self.require(&upstream)?;
        Ok(SearchProjection {
            upstream,
            full: full.clone(),
            filter_digest: full.filter_digest()?,
            deferred_filters: deferred,
            limitations: self.limitations.clone(),
        })
    }
}

/// Trabajo interno para P044; no es una respuesta HTTP ni un cursor firmado.
/// `full` conserva rangos exactos, toggles, cursor federado y filtros sin pérdida.
/// Una página upstream no acredita una búsqueda global completa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchProjection {
    pub upstream: SourceSearch,
    pub full: Search,
    pub filter_digest: String,
    pub deferred_filters: BTreeSet<&'static str>,
    pub limitations: Vec<String>,
}
