//! DTO `Search` completo de §108 (contrato 11/H05) para `POST /models/search`
//! y `POST /sources/search`.
//!
//! Todos los campos son explícitos y obligatorios en la petición (el schema
//! los marca `required`); un array vacío o un límite `null` omite ese filtro y
//! `q` vacío omite el texto. Los elementos de una lista son OR; dimensiones y
//! toggles son AND. Este incremento valida y normaliza el DTO; la búsqueda
//! federada real llega con P044 y no se afirma aquí.

use crate::error::{ErrorCode, LarmError};
use crate::units::DecimalU64;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CompatibilityFilter {
    All,
    Compatible,
    Unknown,
    Incompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CertificationStatus {
    Certified,
    Uncertified,
    Expired,
    Unknown,
}

/// Rango decimal exacto (`DecimalRange`): strings decimales o `null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecimalRange {
    #[serde(deserialize_with = "Option::deserialize")]
    pub minimum: Option<DecimalU64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub maximum: Option<DecimalU64>,
}

/// Rango de puntuación 0..=100 (`ScoreRange`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScoreRange {
    #[serde(deserialize_with = "Option::deserialize")]
    pub minimum: Option<u8>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub maximum: Option<u8>,
}

/// Petición `Search` (schema `Search`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Search {
    pub q: String,
    pub capabilities: Vec<String>,
    pub source_ids: Vec<String>,
    pub compatibility: CompatibilityFilter,
    pub architectures: Vec<String>,
    pub formats: Vec<String>,
    pub quantizations: Vec<String>,
    pub licenses: Vec<String>,
    pub runtime_ids: Vec<String>,
    pub parameter_count: DecimalRange,
    pub size_bytes: DecimalRange,
    pub trust_score: ScoreRange,
    pub certification_statuses: Vec<CertificationStatus>,
    pub only_executable: bool,
    pub only_recommended: bool,
    pub show_incompatible: bool,
    #[serde(deserialize_with = "Option::deserialize")]
    pub cursor: Option<String>,
    pub limit: u16,
}

fn invalid(field: &str, why: &str) -> LarmError {
    LarmError::new(
        ErrorCode::ValidationFailed,
        format!("Search.{field}: {why}"),
        "search_invalid",
    )
    .with_resource(field.to_string())
}

fn unique_nonempty(field: &str, items: &[String]) -> Result<(), LarmError> {
    if items.iter().any(String::is_empty) {
        return Err(invalid(field, "contiene un elemento vacío"));
    }
    let mut seen = std::collections::BTreeSet::new();
    if !items.iter().all(|i| seen.insert(i)) {
        return Err(invalid(field, "elementos repetidos"));
    }
    Ok(())
}

impl Search {
    /// Restricciones del schema que el tipo no expresa.
    ///
    /// # Errors
    /// `VALIDATION_FAILED` con el campo en `details.resource_ids`.
    pub fn validate(&self) -> Result<(), LarmError> {
        if self.q.chars().count() > 500 {
            return Err(invalid("q", "más de 500 caracteres"));
        }
        for (field, items) in [
            ("capabilities", &self.capabilities),
            ("source_ids", &self.source_ids),
            ("architectures", &self.architectures),
            ("formats", &self.formats),
            ("quantizations", &self.quantizations),
            ("licenses", &self.licenses),
            ("runtime_ids", &self.runtime_ids),
        ] {
            unique_nonempty(field, items)?;
        }
        for id in &self.source_ids {
            if !crate::bridge::is_uuid_v7(id) {
                return Err(invalid("source_ids", &format!("{id:.40} no es UUIDv7")));
            }
        }
        for capability in &self.capabilities {
            crate::model::capability(capability)?;
        }
        if let Some(bad) = self
            .runtime_ids
            .iter()
            .find(|id| !crate::bridge::is_uuid_v7(id))
        {
            return Err(invalid("runtime_ids", &format!("{bad:.40} no es UUIDv7")));
        }
        let mut statuses = std::collections::BTreeSet::new();
        if !self
            .certification_statuses
            .iter()
            .all(|s| statuses.insert(*s))
        {
            return Err(invalid("certification_statuses", "elementos repetidos"));
        }
        for (field, range) in [
            ("parameter_count", &self.parameter_count),
            ("size_bytes", &self.size_bytes),
        ] {
            if let (Some(min), Some(max)) = (range.minimum, range.maximum) {
                if min > max {
                    return Err(invalid(field, "minimum > maximum"));
                }
            }
        }
        let t = &self.trust_score;
        if t.minimum.is_some_and(|v| v > 100) || t.maximum.is_some_and(|v| v > 100) {
            return Err(invalid("trust_score", "fuera de 0..=100"));
        }
        if let (Some(min), Some(max)) = (t.minimum, t.maximum) {
            if min > max {
                return Err(invalid("trust_score", "minimum > maximum"));
            }
        }
        if !(1..=200).contains(&self.limit) {
            return Err(invalid("limit", "fuera de 1..=200"));
        }
        if self.cursor.as_deref() == Some("") {
            return Err(invalid("cursor", "vacío; use null para empezar"));
        }
        Ok(())
    }

    /// Resumen de todos los filtros salvo `cursor` y `limit` (H05): cambiar
    /// cualquier filtro invalida el cursor. Listas como conjuntos (OR): el
    /// orden de sus elementos no cambia la búsqueda.
    ///
    /// # Errors
    /// `INTERNAL` si la serialización canónica falla.
    pub fn filter_digest(&self) -> Result<String, LarmError> {
        let mut normalized = self.clone();
        for list in [
            &mut normalized.capabilities,
            &mut normalized.source_ids,
            &mut normalized.architectures,
            &mut normalized.formats,
            &mut normalized.quantizations,
            &mut normalized.licenses,
            &mut normalized.runtime_ids,
        ] {
            list.sort();
            list.dedup();
        }
        normalized.certification_statuses.sort();
        let mut value = serde_json::to_value(&normalized).map_err(|e| {
            LarmError::new(ErrorCode::Internal, "Search no serializable", "serialize")
                .with_technical(e.to_string())
        })?;
        if let Some(map) = value.as_object_mut() {
            map.remove("cursor");
            map.remove("limit");
        }
        crate::canonical::sha256_hex(&value).map_err(|e| {
            LarmError::new(
                ErrorCode::Internal,
                "Search no canonicalizable",
                "canonical",
            )
            .with_technical(e)
        })
    }

    /// Contradicción explícita de H05: se pide solo INCOMPATIBLE pero se ocultan
    /// los incompatibles. El resultado es vacío y la UI la muestra; el filtro no se cambia.
    #[must_use]
    pub fn is_contradictory(&self) -> bool {
        self.compatibility == CompatibilityFilter::Incompatible && !self.show_incompatible
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use serde_json::json;

    fn full() -> serde_json::Value {
        json!({
            "q": "qwen", "capabilities": ["text.generate", "vision.understand"], "source_ids": ["0192aaaa-0000-7000-8000-000000000001", "0192aaaa-0000-7000-8000-000000000002"],
            "compatibility": "COMPATIBLE", "architectures": ["qwen3"], "formats": ["gguf", "onnx"],
            "quantizations": ["Q4_K_M"], "licenses": ["apache-2.0"],
            "runtime_ids": ["0192aaaa-0000-7000-8000-000000000001"],
            "parameter_count": {"minimum": "500000000", "maximum": "9000000000"},
            "size_bytes": {"minimum": null, "maximum": "20000000000"},
            "trust_score": {"minimum": 50, "maximum": null},
            "certification_statuses": ["CERTIFIED", "UNCERTIFIED"],
            "only_executable": true, "only_recommended": false, "show_incompatible": false,
            "cursor": null, "limit": 50
        })
    }

    fn with(field: &str, value: serde_json::Value) -> serde_json::Value {
        let mut v = full();
        v[field] = value;
        v
    }

    #[test]
    fn acepta_todos_los_filtros_rangos_y_toggles() {
        let s: Search = serde_json::from_value(full()).unwrap();
        s.validate().unwrap();
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            full(),
            "roundtrip exacto"
        );
        let minimal: Search = serde_json::from_value(json!({
            "q": "", "capabilities": [], "source_ids": [], "compatibility": "ALL", "architectures": [],
            "formats": [], "quantizations": [], "licenses": [], "runtime_ids": [],
            "parameter_count": {"minimum": null, "maximum": null}, "size_bytes": {"minimum": null, "maximum": null},
            "trust_score": {"minimum": null, "maximum": null}, "certification_statuses": [],
            "only_executable": false, "only_recommended": false, "show_incompatible": true, "cursor": null, "limit": 1
        }))
        .unwrap();
        minimal.validate().unwrap();
    }

    #[test]
    fn rechaza_propiedades_tipos_y_campos_ausentes() {
        // `Option` expresa null; no vuelve opcional una clave required del schema.
        for field in full().as_object().unwrap().keys() {
            let mut missing = full();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<Search>(missing).is_err(),
                "{field}"
            );
        }
        for field in ["parameter_count", "size_bytes", "trust_score"] {
            for bound in ["minimum", "maximum"] {
                let mut missing = full();
                missing[field].as_object_mut().unwrap().remove(bound);
                assert!(
                    serde_json::from_value::<Search>(missing).is_err(),
                    "{field}.{bound}"
                );
            }
        }
        let mut extra = full();
        extra["popularity"] = json!(5);
        let mut missing = full();
        missing.as_object_mut().unwrap().remove("show_incompatible");
        for bad in [
            extra,
            missing,
            with("compatibility", json!("MAYBE")),
            with("limit", json!("50")),
            with("parameter_count", json!({"minimum": 5, "maximum": null})),
            with(
                "size_bytes",
                json!({"minimum": "1", "maximum": null, "step": "1"}),
            ),
            with("trust_score", json!({"minimum": "10", "maximum": null})),
            with("certification_statuses", json!(["VALID"])),
            with("only_executable", json!("true")),
        ] {
            assert!(
                serde_json::from_value::<Search>(bad.clone()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn valida_rangos_limites_y_listas() {
        for (field, value, reason_field) in [
            (
                "parameter_count",
                json!({"minimum": "10", "maximum": "9"}),
                "parameter_count",
            ),
            (
                "trust_score",
                json!({"minimum": 101, "maximum": null}),
                "trust_score",
            ),
            (
                "trust_score",
                json!({"minimum": 80, "maximum": 20}),
                "trust_score",
            ),
            ("limit", json!(0), "limit"),
            ("limit", json!(201), "limit"),
            ("formats", json!(["gguf", "gguf"]), "formats"),
            ("architectures", json!([""]), "architectures"),
            (
                "runtime_ids",
                json!(["8f14e45f-ceea-4c3b-9e1d-2a7b5c3d4e5f"]),
                "runtime_ids",
            ),
            ("cursor", json!(""), "cursor"),
            ("q", json!("x".repeat(501)), "q"),
        ] {
            let s: Search = serde_json::from_value(with(field, value)).unwrap();
            let e = s.validate().unwrap_err();
            assert_eq!(e.details.resource_ids, vec![reason_field.to_string()]);
        }
    }

    #[test]
    fn digest_ignora_cursor_limit_y_orden_de_listas_pero_no_filtros() {
        let a: Search = serde_json::from_value(full()).unwrap();
        let mut b = a.clone();
        b.cursor = Some("c".into());
        b.limit = 10;
        b.formats.reverse();
        assert_eq!(a.filter_digest().unwrap(), b.filter_digest().unwrap());
        let mut c = a.clone();
        c.only_recommended = true;
        assert_ne!(a.filter_digest().unwrap(), c.filter_digest().unwrap());
        let mut d = a.clone();
        d.size_bytes.minimum = DecimalU64::new(1);
        assert_ne!(a.filter_digest().unwrap(), d.filter_digest().unwrap());
    }

    #[test]
    fn contradiccion_incompatible_oculto_se_detecta() {
        let mut s: Search = serde_json::from_value(full()).unwrap();
        s.compatibility = CompatibilityFilter::Incompatible;
        assert!(s.is_contradictory());
        s.show_incompatible = true;
        assert!(!s.is_contradictory());
    }
}
