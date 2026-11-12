//! Identificadores de correlación propagables API → core → adapter worker (P003).

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Correlation ID: se genera en la entrada de la API y acompaña toda la
/// operación (logs de core y de workers de adaptador lo referencian).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CorrelationId(Uuid);

impl CorrelationId {
    /// Genera un nuevo id (UUID v7, ordenable por tiempo, ADR-0007).
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Construye desde su representación textual (p. ej. cabecera entrante).
    /// Solo acepta UUIDv7 en minúsculas, la forma que exige el schema `Error`.
    pub fn parse(s: &str) -> Result<Self, String> {
        if !crate::bridge::is_uuid_v7(s) {
            return Err(format!(
                "correlation id no es UUIDv7 en minúsculas: {s:.64}"
            ));
        }
        Uuid::parse_str(s).map(Self).map_err(|e| e.to_string())
    }
}

impl Default for CorrelationId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for CorrelationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Nombre de cabecera HTTP para propagar la correlación.
pub const CORRELATION_HEADER: &str = "x-correlation-id";

/// Propaga el id recibido o genera uno nuevo si no venía (comportamiento de entrada de API).
#[must_use]
pub fn ensure_correlation(incoming: Option<CorrelationId>) -> CorrelationId {
    incoming.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genera_parsea_y_roundtrip() {
        let id = CorrelationId::new();
        let s = id.to_string();
        let back = CorrelationId::parse(&s).unwrap_or_else(|_| unreachable!());
        assert_eq!(id, back);
    }

    #[test]
    fn rechaza_lo_que_no_es_uuidv7() {
        for bad in [
            "8f14e45f-ceea-4c3b-9e1d-2a7b5c3d4e5f", // v4
            "0192AAAA-0000-7000-8000-000000000000", // mayúsculas
            "no-es-uuid",
            "",
        ] {
            assert!(CorrelationId::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn entrada_ausente_genera_nuevo_y_presente_se_conserva() {
        let existing = CorrelationId::new();
        assert_ne!(ensure_correlation(None), ensure_correlation(None));
        assert_eq!(ensure_correlation(Some(existing)), existing);
    }

    #[test]
    fn se_serializa_como_string_en_json() {
        let id = CorrelationId::new();
        let j = serde_json::to_string(&id).unwrap_or_default();
        let back: CorrelationId = serde_json::from_str(&j).unwrap_or_else(|_| unreachable!());
        assert_eq!(id, back);
    }
}
