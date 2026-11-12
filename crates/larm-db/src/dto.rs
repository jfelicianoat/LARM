//! Frontera entre los tipos del dominio y las columnas SQLite (P004).
//!
//! Los bytes viajan en JSON como string decimal ([`DecimalU64`], contrato 00)
//! y se guardan en SQLite como INTEGER con signo: el máximo común es
//! `i64::MAX`. Una fila con un valor negativo o fuera de rango es una base
//! corrupta, no un dato que convertir en silencio. Ningún `SqliteRow` sale de
//! este crate: los repositorios devuelven tipos del dominio.

use larm_domain::error::{ErrorCode, LarmError};
use larm_domain::units::DecimalU64;

/// Bytes del dominio a columna INTEGER (siempre cabe: `DecimalU64` ≤ i64::MAX).
#[must_use]
pub fn bytes_to_sql(value: DecimalU64) -> i64 {
    i64::try_from(value.get()).unwrap_or(i64::MAX)
}

/// Columna INTEGER a bytes del dominio.
///
/// # Errors
/// `INTEGRITY_FAILED` si la columna contiene un valor negativo.
pub fn bytes_from_sql(column: &str, value: i64) -> Result<DecimalU64, LarmError> {
    u64::try_from(value)
        .ok()
        .and_then(DecimalU64::new)
        .ok_or_else(|| {
            LarmError::new(
                ErrorCode::IntegrityFailed,
                "La base de datos contiene un tamaño inválido",
                "negative_byte_count",
            )
            .with_technical(format!("{column}={value}"))
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn roundtrip_hasta_i64_max() {
        for raw in [
            0u64,
            2_199_023_255_552,
            9_007_199_254_740_993,
            i64::MAX as u64,
        ] {
            let v = DecimalU64::new(raw).unwrap();
            assert_eq!(bytes_from_sql("c", bytes_to_sql(v)).unwrap(), v);
        }
    }

    #[test]
    fn negativo_en_la_base_es_integridad_fallida() {
        let e = bytes_from_sql("capacity_bytes", -1).unwrap_err();
        assert_eq!(e.code, ErrorCode::IntegrityFailed);
    }
}
