//! Representaciones comunes del contrato 00.
//!
//! Tamaños `u64` internos limitados a 9223372036854775807 (máximo de SQLite) y
//! representados en JSON como string decimal, nunca como `number` de JS. La
//! entrada es estricta: un número JSON, ceros a la izquierda, signo o un valor
//! por encima del máximo se rechazan antes de cualquier IO.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Máximo representable (SQLite INTEGER con signo).
pub const DECIMAL_U64_MAX: u64 = i64::MAX as u64;

/// Entero sin signo serializado como string decimal (`^(0|[1-9][0-9]*)$`, ≤ i64::MAX).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct DecimalU64(u64);

impl DecimalU64 {
    /// `None` si supera [`DECIMAL_U64_MAX`].
    #[must_use]
    pub fn new(value: u64) -> Option<Self> {
        (value <= DECIMAL_U64_MAX).then_some(Self(value))
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }

    /// Parsea la representación textual del contrato.
    pub fn parse(text: &str) -> Result<Self, String> {
        let canonical = !text.is_empty()
            && text.len() <= 19
            && text.bytes().all(|b| b.is_ascii_digit())
            && (text == "0" || !text.starts_with('0'));
        if !canonical {
            return Err(format!(
                "se esperaba un entero decimal sin signo ni ceros a la izquierda, recibido {text:?}"
            ));
        }
        text.parse::<u64>()
            .ok()
            .and_then(Self::new)
            .ok_or_else(|| format!("{text} supera el máximo {DECIMAL_U64_MAX}"))
    }
}

impl fmt::Display for DecimalU64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Serialize for DecimalU64 {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for DecimalU64 {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = DecimalU64;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("un string decimal (contrato 00: bytes como string, nunca number)")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<DecimalU64, E> {
                DecimalU64::parse(v).map_err(E::custom)
            }
        }
        deserializer.deserialize_str(Visitor)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn roundtrip_como_string() {
        let v: DecimalU64 = serde_json::from_str("\"10737418240\"").unwrap();
        assert_eq!(v.get(), 10_737_418_240);
        assert_eq!(serde_json::to_string(&v).unwrap(), "\"10737418240\"");
        let max: DecimalU64 = serde_json::from_str("\"9223372036854775807\"").unwrap();
        assert_eq!(max.get(), DECIMAL_U64_MAX);
    }

    #[test]
    fn rechaza_number_signo_ceros_y_overflow() {
        for bad in [
            "1024",                     // number JSON
            "\"\"",                     // vacío
            "\"0012\"",                 // ceros a la izquierda
            "\"-5\"",                   // signo
            "\"+5\"",                   // signo
            "\"1e3\"",                  // notación
            "\"9223372036854775808\"",  // i64::MAX + 1
            "\"18446744073709551615\"", // u64::MAX
        ] {
            assert!(serde_json::from_str::<DecimalU64>(bad).is_err(), "{bad}");
        }
        assert!(DecimalU64::new(u64::MAX).is_none());
    }
}
