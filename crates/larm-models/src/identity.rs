//! Resolución de identidad entre dos modelos (P041; spec §18, contrato 01).
//!
//! Grados: `IDENTICAL`, `PROBABLE_SAME_MODEL`, `DERIVED_MODEL`, `UNRELATED`
//! y `UNKNOWN`. Solo `IDENTICAL` permite agrupar automáticamente, y solo
//! con evidencia fuerte: la misma identidad upstream explícita, o los mismos
//! hashes de todos los artefactos requeridos con la misma configuración y el
//! mismo tokenizador. Un nombre parecido nunca basta. `DERIVED_MODEL`
//! conserva el linaje y `PROBABLE_SAME_MODEL` se publica como posible
//! duplicado para revisión, sin fusionar.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IdentityGrade {
    Identical,
    ProbableSameModel,
    DerivedModel,
    Unrelated,
    Unknown,
}

/// Evidencia de identidad de un modelo (§18). Todo opcional: lo que no se
/// conoce no se inventa.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityEvidence {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub upstream_identity: Option<String>,
    #[serde(default)]
    pub architecture: Option<String>,
    #[serde(default)]
    pub base_model: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    /// sha256 de la configuración (config.json o metadata del GGUF).
    #[serde(default)]
    pub config_hash: Option<String>,
    #[serde(default)]
    pub tokenizer_hash: Option<String>,
    /// sha256 de los artefactos requeridos.
    #[serde(default)]
    pub required_hashes: BTreeSet<String>,
    /// sha256 de los artefactos de los que deriva (linaje de conversión).
    #[serde(default)]
    pub parent_hashes: BTreeSet<String>,
}

/// Resultado: grado, motivos y evidencias usadas (se guardan).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityResolution {
    pub grade: IdentityGrade,
    pub reasons: Vec<String>,
    /// Si se puede agrupar sin revisión humana (solo IDENTICAL).
    pub auto_merge: bool,
}

fn same(a: Option<&String>, b: Option<&String>) -> Option<bool> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.eq_ignore_ascii_case(y)),
        _ => None,
    }
}

fn normalized_name(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn resolution(grade: IdentityGrade, reasons: Vec<String>) -> IdentityResolution {
    IdentityResolution {
        auto_merge: grade == IdentityGrade::Identical,
        grade,
        reasons,
    }
}

/// Compara dos modelos. Simétrica salvo en los motivos de linaje.
#[must_use]
pub fn resolve(a: &IdentityEvidence, b: &IdentityEvidence) -> IdentityResolution {
    // 1. Identidad upstream explícita: la misma es el mismo modelo.
    if same(a.upstream_identity.as_ref(), b.upstream_identity.as_ref()) == Some(true) {
        return resolution(
            IdentityGrade::Identical,
            vec!["upstream_identity_equal".into()],
        );
    }
    // 2. Los mismos bytes requeridos con la misma configuración y tokenizador.
    let same_bytes = !a.required_hashes.is_empty() && a.required_hashes == b.required_hashes;
    if same_bytes && a.config_hash == b.config_hash && a.tokenizer_hash == b.tokenizer_hash {
        return resolution(
            IdentityGrade::Identical,
            vec![
                "required_hashes_equal".into(),
                "config_tokenizer_equal".into(),
            ],
        );
    }
    // 3. Linaje: uno declara al otro como base o deriva de sus bytes.
    let mut derived = Vec::new();
    for (x, y, dir) in [(a, b, "a_from_b"), (b, a, "b_from_a")] {
        if same(x.base_model.as_ref(), y.upstream_identity.as_ref()) == Some(true) {
            derived.push(format!("base_model_points_to_other:{dir}"));
        }
        if !x.parent_hashes.is_disjoint(&y.required_hashes) {
            derived.push(format!("lineage_parent_hash:{dir}"));
        }
    }
    if !derived.is_empty() {
        return resolution(IdentityGrade::DerivedModel, derived);
    }
    // 4. Evidencia de que son distintos.
    let mut unrelated = Vec::new();
    if same(a.architecture.as_ref(), b.architecture.as_ref()) == Some(false) {
        unrelated.push("architecture_differs".into());
    }
    if same(a.upstream_identity.as_ref(), b.upstream_identity.as_ref()) == Some(false)
        && same(a.base_model.as_ref(), b.base_model.as_ref()) != Some(true)
    {
        unrelated.push("upstream_identity_differs".into());
    }
    if same_bytes {
        // Mismos pesos pero otra configuración o tokenizador: dudoso, no distinto.
        return resolution(
            IdentityGrade::ProbableSameModel,
            vec![
                "required_hashes_equal".into(),
                "config_or_tokenizer_differs".into(),
            ],
        );
    }
    if !unrelated.is_empty() {
        return resolution(IdentityGrade::Unrelated, unrelated);
    }
    // 5. Indicios de que podrían ser el mismo: nunca fusión automática.
    let mut probable = Vec::new();
    let same_arch = same(a.architecture.as_ref(), b.architecture.as_ref()) == Some(true);
    for (field, x, y) in [
        ("base_model", &a.base_model, &b.base_model),
        ("author", &a.author, &b.author),
        ("config_hash", &a.config_hash, &b.config_hash),
        ("tokenizer_hash", &a.tokenizer_hash, &b.tokenizer_hash),
    ] {
        if same(x.as_ref(), y.as_ref()) == Some(true) {
            probable.push(format!("{field}_equal"));
        }
    }
    let same_name = !a.name.is_empty() && normalized_name(&a.name) == normalized_name(&b.name);
    if same_arch && (!probable.is_empty() || same_name) {
        probable.insert(0, "architecture_equal".into());
        if same_name {
            probable.push("name_similar".into());
        }
        return resolution(IdentityGrade::ProbableSameModel, probable);
    }
    if same_name {
        return resolution(
            IdentityGrade::Unknown,
            vec!["name_only".into(), "insufficient_evidence".into()],
        );
    }
    resolution(IdentityGrade::Unknown, vec!["insufficient_evidence".into()])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn h(c: char) -> String {
        c.to_string().repeat(64)
    }

    fn qwen() -> IdentityEvidence {
        IdentityEvidence {
            name: "Qwen-X 7B".into(),
            upstream_identity: None,
            architecture: Some("qwen2".into()),
            base_model: None,
            author: Some("Qwen".into()),
            config_hash: Some(h('c')),
            tokenizer_hash: Some(h('t')),
            required_hashes: [h('1'), h('2')].into_iter().collect(),
            parent_hashes: BTreeSet::new(),
        }
    }

    #[test]
    fn hashes_identicos() {
        let mut mirror = qwen();
        mirror.name = "otro nombre cualquiera".into();
        let r = resolve(&qwen(), &mirror);
        assert_eq!(r.grade, IdentityGrade::Identical);
        assert!(r.auto_merge);
        assert!(r.reasons.contains(&"required_hashes_equal".to_string()));
        // Mismos pesos con otro tokenizador: dudoso, sin fusión.
        let mut tok = qwen();
        tok.tokenizer_hash = Some(h('u'));
        let r = resolve(&qwen(), &tok);
        assert_eq!(r.grade, IdentityGrade::ProbableSameModel);
        assert!(!r.auto_merge);
    }

    #[test]
    fn upstream_explicito() {
        let a = IdentityEvidence {
            upstream_identity: Some("hf:Qwen/Qwen-X".into()),
            ..IdentityEvidence::default()
        };
        let b = a.clone();
        assert_eq!(resolve(&a, &b).grade, IdentityGrade::Identical);
    }

    #[test]
    fn mismo_nombre_modelo_distinto() {
        let mut other = qwen();
        other.architecture = Some("llama".into());
        other.required_hashes = [h('9')].into_iter().collect();
        other.config_hash = Some(h('8'));
        other.tokenizer_hash = Some(h('7'));
        other.author = Some("alguien".into());
        let r = resolve(&qwen(), &other);
        assert_eq!(r.grade, IdentityGrade::Unrelated);
        assert_eq!(r.reasons, vec!["architecture_differs".to_string()]);
        assert!(!r.auto_merge);
        // Solo el nombre: desconocido, nunca fusión.
        let a = IdentityEvidence {
            name: "Qwen-X".into(),
            ..IdentityEvidence::default()
        };
        let b = IdentityEvidence {
            name: "qwen x".into(),
            ..IdentityEvidence::default()
        };
        let r = resolve(&a, &b);
        assert_eq!(r.grade, IdentityGrade::Unknown);
        assert_eq!(r.reasons[0], "name_only");
    }

    #[test]
    fn cuantizacion_derivada() {
        let mut base = qwen();
        base.upstream_identity = Some("hf:Qwen/Qwen-X".into());
        let quant = IdentityEvidence {
            name: "Qwen-X-GGUF".into(),
            upstream_identity: Some("hf:bartowski/Qwen-X-GGUF".into()),
            architecture: Some("qwen2".into()),
            base_model: Some("hf:Qwen/Qwen-X".into()),
            required_hashes: [h('q')].into_iter().collect(),
            parent_hashes: [h('1')].into_iter().collect(),
            ..IdentityEvidence::default()
        };
        let r = resolve(&quant, &base);
        assert_eq!(r.grade, IdentityGrade::DerivedModel);
        assert!(r
            .reasons
            .contains(&"base_model_points_to_other:a_from_b".to_string()));
        assert!(r
            .reasons
            .contains(&"lineage_parent_hash:a_from_b".to_string()));
        assert!(
            !r.auto_merge,
            "una derivada conserva su linaje y no se fusiona"
        );
        // Simétrica en el grado.
        assert_eq!(resolve(&base, &quant).grade, IdentityGrade::DerivedModel);
    }

    #[test]
    fn probable_sin_fusion() {
        let mut fine_tune = qwen();
        fine_tune.required_hashes = [h('5')].into_iter().collect();
        fine_tune.config_hash = Some(h('6'));
        fine_tune.tokenizer_hash = Some(h('t'));
        let r = resolve(&qwen(), &fine_tune);
        assert_eq!(r.grade, IdentityGrade::ProbableSameModel);
        assert!(!r.auto_merge);
        assert_eq!(
            r.reasons,
            vec![
                "architecture_equal".to_string(),
                "author_equal".to_string(),
                "tokenizer_hash_equal".to_string(),
                "name_similar".to_string()
            ]
        );
        // Sin evidencia ninguna.
        let r = resolve(&IdentityEvidence::default(), &IdentityEvidence::default());
        assert_eq!(r.grade, IdentityGrade::Unknown);
    }

    #[test]
    fn serializacion() {
        assert_eq!(
            serde_json::to_value(IdentityGrade::ProbableSameModel).unwrap(),
            "PROBABLE_SAME_MODEL"
        );
        let e = qwen();
        let back: IdentityEvidence =
            serde_json::from_value(serde_json::to_value(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }
}
