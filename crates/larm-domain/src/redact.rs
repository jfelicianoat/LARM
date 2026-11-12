//! Redacción de secretos para logs, diagnósticos y `original_error` (P003, contrato 07).
//!
//! Prohibido loguear `Authorization`, `Cookie` o API keys: los patrones
//! conocidos se sustituyen por `[REDACTED]` antes de escribir. Los tokens de
//! LARM son hexadecimal sin prefijo, indistinguibles de un sha256 que sí debe
//! verse, así que se redactan por su contexto (`Bearer`, `token=`), no por forma.
//! Vive en el dominio (puro, sin IO) para que un error se construya ya redactado.

use regex::Regex;
use std::sync::OnceLock;

/// Patrón y sustitución; `${1}` conserva el nombre de la clave.
fn patterns() -> &'static Vec<(Regex, &'static str)> {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            // Cabeceras sensibles completas
            (
                r#"(?i)\b(authorization|cookie|set-cookie|proxy-authorization|x-api-key)\s*:\s*\S[^,\r\n]*"#,
                "[REDACTED]",
            ),
            // Esquema Bearer fuera de una cabecera
            (r"(?i)\bbearer\s+[A-Za-z0-9\-._~+/=]{8,}", "[REDACTED]"),
            // Formas clave=valor / "key": "value"
            (
                r#"(?i)((?:api[_-]?key|apikey|access[_-]?token|token|secret|password|passwd|client[_-]?secret)"?\s*[=:]\s*\$?"?)[A-Za-z0-9\-._~+/=]{6,}"#,
                "${1}[REDACTED]",
            ),
            // Tokens con prefijo conocido
            (r"\bsk-[A-Za-z0-9_-]{16,}", "[REDACTED]"),
            (r"\bghp_[A-Za-z0-9]{30,}", "[REDACTED]"),
            (r"\bgithub_pat_[A-Za-z0-9_]{30,}", "[REDACTED]"),
            (r"\bhf_[A-Za-z0-9]{30,}", "[REDACTED]"),
            (r"\bAKIA[0-9A-Z]{16}\b", "[REDACTED]"),
            // JWT
            (
                r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
                "[REDACTED]",
            ),
        ]
        .into_iter()
        .filter_map(|(re, rep)| Regex::new(re).ok().map(|re| (re, rep)))
        .collect()
    })
}

/// Número de patrones compilados; un patrón inválido no se compila y lo detecta el test.
#[must_use]
pub fn pattern_count() -> usize {
    patterns().len()
}

/// Devuelve `input` con todo secreto reconocido sustituido por `[REDACTED]`.
#[must_use]
pub fn redact(input: &str) -> String {
    let mut out = input.to_string();
    for (re, rep) in patterns() {
        out = re.replace_all(&out, *rep).into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todos_los_patrones_compilan() {
        assert_eq!(pattern_count(), 9);
    }

    #[test]
    fn redacta_patrones_conocidos() {
        assert_eq!(
            redact("Authorization: Bearer abc.def.ghi-jkl"),
            "[REDACTED]"
        );
        assert_eq!(redact("cookie: sid=abc; theme=dark"), "[REDACTED]");
        assert_eq!(
            redact("enviando bearer 0123456789abcdef0123"),
            "enviando [REDACTED]"
        );
        assert_eq!(redact("sk-1234567890abcdefghijklmnop"), "[REDACTED]");
        assert_eq!(redact("ghp_abcdefghijklmnopqrstuvwxyz012345"), "[REDACTED]");
        assert_eq!(
            redact("hf_abcdefghijklmnopqrstuvwxyz0123456789"),
            "[REDACTED]"
        );
        assert_eq!(redact("AKIAIOSFODNN7EXAMPLE"), "[REDACTED]");
        assert_eq!(redact("api_key=supersecreto123"), "api_key=[REDACTED]");
        assert_eq!(
            redact(r#"{"token": "abcdef123456"}"#),
            r#"{"token": "[REDACTED]"}"#
        );
    }

    #[test]
    fn no_altera_texto_normal_ni_hashes() {
        for normal in [
            "descarga completada bytes=1048 modelo=Qwen3-0.6B-GGUF",
            "GET /api/v1/models?page=2 HTTP/1.1 200",
            "sha256=9465e63a22add5354d9bb4b99e90117043c7124007664907259bd16d043bb031",
        ] {
            assert_eq!(redact(normal), normal);
        }
    }
}
