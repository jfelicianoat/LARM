//! JSON canónico RFC 8785 (JCS) y sha256 sobre él (contrato 00).
//!
//! Claves ordenadas por unidades UTF-16, sin espacios y strings con el escape
//! mínimo de JSON. Los números siguen `Number.prototype.toString` de
//! ECMAScript (RFC 8785 §3.2.2.3) sobre su valor IEEE-754 doble: dígitos
//! mínimos que reproducen el valor y notación exponencial fuera de
//! [1e-6, 1e21). Un entero que un doble no representa exactamente (|n| > 2^53)
//! se rechaza en vez de alterarse en silencio; los bytes viajan como string.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Serialización canónica de `value`.
///
/// # Errors
/// Texto del motivo si `value` contiene un número no entero.
pub fn canonicalize(value: &Value) -> Result<String, String> {
    let mut out = String::new();
    write(value, &mut out)?;
    Ok(out)
}

/// sha256 hexadecimal en minúsculas de la forma canónica.
///
/// # Errors
/// Ver [`canonicalize`].
pub fn sha256_hex(value: &Value) -> Result<String, String> {
    Ok(hex_lower(&Sha256::digest(canonicalize(value)?.as_bytes())))
}

fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 0xf)]));
    }
    s
}

fn write(value: &Value, out: &mut String) -> Result<(), String> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            const EXACT: u64 = 1 << 53;
            if let Some(i) = n.as_i64() {
                if i.unsigned_abs() > EXACT {
                    return Err(format!(
                        "entero {i} fuera del rango exacto de un doble (±2^53)"
                    ));
                }
                out.push_str(&i.to_string());
            } else if let Some(u) = n.as_u64() {
                if u > EXACT {
                    return Err(format!(
                        "entero {u} fuera del rango exacto de un doble (±2^53)"
                    ));
                }
                out.push_str(&u.to_string());
            } else {
                let f = n
                    .as_f64()
                    .ok_or_else(|| format!("número {n} no representable"))?;
                out.push_str(&ecmascript_number(f)?);
            }
        }
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write(&map[*key], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// `Number.prototype.toString(x)` (ECMA-262, Number::toString) para un doble
/// finito. Los dígitos mínimos salen del formato `{:e}` de Rust, que da la
/// representación más corta que vuelve a leerse como el mismo doble.
///
/// # Errors
/// NaN e infinito no tienen representación JSON.
pub fn ecmascript_number(x: f64) -> Result<String, String> {
    if !x.is_finite() {
        return Err(format!("{x} no es representable en JSON"));
    }
    if x == 0.0 {
        return Ok("0".into()); // también -0
    }
    if x < 0.0 {
        return Ok(format!("-{}", ecmascript_number(-x)?));
    }
    // `{:e}`: "d.ddde±x" o "de±x" con los dígitos mínimos.
    let sci = format!("{x:e}");
    let (mantissa, exponent) = sci
        .split_once('e')
        .ok_or_else(|| format!("formato inesperado {sci}"))?;
    let exponent: i32 = exponent
        .parse()
        .map_err(|_| format!("exponente inesperado {sci}"))?;
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    // x = 0.d1d2…dk × 10^n
    let k = i32::try_from(digits.len()).map_err(|_| "demasiados dígitos".to_string())?;
    let n = exponent + 1;
    let out = if k <= n && n <= 21 {
        format!(
            "{digits}{}",
            "0".repeat(usize::try_from(n - k).unwrap_or(0))
        )
    } else if 0 < n && n <= 21 {
        let (int, frac) = digits.split_at(usize::try_from(n).unwrap_or(0));
        format!("{int}.{frac}")
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat(usize::try_from(-n).unwrap_or(0)))
    } else {
        let e = n - 1;
        let sign = if e < 0 { '-' } else { '+' };
        let (first, rest) = digits.split_at(1);
        if rest.is_empty() {
            format!("{first}e{sign}{}", e.abs())
        } else {
            format!("{first}.{rest}e{sign}{}", e.abs())
        }
    };
    Ok(out)
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn orden_de_claves_por_utf16_y_sin_espacios() {
        // Ejemplo de RFC 8785 §3.2.3: "\u{20ac}" (U+20AC) va antes que "\u{1f600}" (par sustituto D83D…).
        let v = json!({"b": 1, "a": [true, null, "x"], "\u{1f600}": 2, "\u{20ac}": 3, "\r": 4});
        assert_eq!(
            canonicalize(&v).unwrap(),
            "{\"\\r\":4,\"a\":[true,null,\"x\"],\"b\":1,\"\u{20ac}\":3,\"\u{1f600}\":2}"
        );
    }

    #[test]
    fn mismo_contenido_mismo_hash_aunque_cambie_el_orden() {
        let a = json!({"x": {"b": 2, "a": 1}, "ids": ["1", "2"]});
        let b: Value = serde_json::from_str(r#"{"ids":["1","2"],"x":{"a":1,"b":2}}"#).unwrap();
        assert_eq!(sha256_hex(&a).unwrap(), sha256_hex(&b).unwrap());
        assert_eq!(sha256_hex(&a).unwrap().len(), 64);
        assert_ne!(
            sha256_hex(&a).unwrap(),
            sha256_hex(&json!({"x": 1})).unwrap()
        );
    }

    #[test]
    fn numeros_segun_rfc_8785_apendice_b() {
        // (bits IEEE-754, serialización esperada) del apéndice B de RFC 8785.
        let cases: [(u64, &str); 12] = [
            (0x0000_0000_0000_0000, "0"),
            (0x8000_0000_0000_0000, "0"),
            (0x0000_0000_0000_0001, "5e-324"),
            (0x8000_0000_0000_0001, "-5e-324"),
            (0x7fef_ffff_ffff_ffff, "1.7976931348623157e+308"),
            (0xffef_ffff_ffff_ffff, "-1.7976931348623157e+308"),
            (0x4340_0000_0000_0000, "9007199254740992"),
            (0xc340_0000_0000_0000, "-9007199254740992"),
            (0x4430_0000_0000_0000, "295147905179352830000"),
            (0x44b5_2d02_c7e1_4af5, "9.999999999999997e+22"),
            (0x44b5_2d02_c7e1_4af6, "1e+23"),
            (0x41b3_de43_5555_5555, "333333333.3333333"),
        ];
        for (bits, expected) in cases {
            assert_eq!(
                ecmascript_number(f64::from_bits(bits)).unwrap(),
                expected,
                "{bits:#018x}"
            );
        }
        let more: [(u64, &str); 4] = [
            (0x3eb0_c6f7_a0b5_ed8d, "0.000001"),
            (0x3eb0_c6f7_a0b5_ed8c, "9.999999999999997e-7"),
            (0x4415_af1d_78b5_8c40, "100000000000000000000"),
            (0x4415_af1d_78b5_8c3f, "99999999999999980000"),
        ];
        for (bits, expected) in more {
            assert_eq!(
                ecmascript_number(f64::from_bits(bits)).unwrap(),
                expected,
                "{bits:#018x}"
            );
        }
        assert_eq!(ecmascript_number(1e21).unwrap(), "1e+21");
        assert!(ecmascript_number(f64::NAN).is_err() && ecmascript_number(f64::INFINITY).is_err());
    }

    #[test]
    fn numeros_en_documentos() {
        assert_eq!(
            canonicalize(&json!({"t": 0.7, "p": 1.0, "n": -7})).unwrap(),
            r#"{"n":-7,"p":1,"t":0.7}"#
        );
        assert!(canonicalize(&json!(9_007_199_254_740_993_u64)).is_err());
        assert!(canonicalize(&json!(-9_007_199_254_740_993_i64)).is_err());
        assert_eq!(
            canonicalize(&json!(9_007_199_254_740_992_u64)).unwrap(),
            "9007199254740992"
        );
    }
}
