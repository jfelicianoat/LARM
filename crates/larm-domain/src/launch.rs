//! Parámetros de lanzamiento y su hash reproducible (P013; spec §56,
//! contrato 11 H03–H04, schema `LaunchParameters`).
//!
//! El hash del perfil es sha256 del JSON RFC 8785 de los parámetros
//! **efectivos**. Incluye argumentos, opciones del adaptador, el entorno no
//! secreto y las referencias de credencial, nunca un secreto. Reordenar las
//! claves de un objeto no lo cambia; cambiar un argumento, una opción, una
//! variable o el límite de memoria sí. El orden de `runtime_arguments` es
//! significativo: son argumentos de proceso.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::units::DecimalU64;

/// Generación (schema `Generation`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generation {
    #[serde(deserialize_with = "Option::deserialize")]
    pub temperature: Option<f64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub top_p: Option<f64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub seed: Option<u64>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub max_output_tokens: Option<u32>,
}

/// Variable de entorno: exactamente un valor no secreto o una referencia de
/// credencial bajo `larm/` (schema `EnvironmentBinding`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum EnvironmentBinding {
    Value {
        name: String,
        value: String,
    },
    Credential {
        name: String,
        credential_reference: String,
    },
}

impl EnvironmentBinding {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Value { name, .. } | Self::Credential { name, .. } => name,
        }
    }
}

/// Entorno (schema `EnvironmentConfig`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentConfig {
    pub allowlist: Vec<String>,
    pub variables: Vec<EnvironmentBinding>,
}

impl EnvironmentConfig {
    /// Reglas comunes del contrato 11 (H04): nombres válidos, sin duplicados
    /// ni variantes de mayúsculas, cada variable en la allowlist y las
    /// credenciales bajo `larm/`.
    ///
    /// # Errors
    /// Primera regla violada.
    pub fn validate(&self) -> Result<(), LaunchInvalid> {
        let env = self;
        let mut allowed = std::collections::BTreeSet::new();
        for name in &env.allowlist {
            if !valid_env_name(name) {
                return Err(invalid(
                    "env_name_invalid",
                    format!("nombre de variable no válido en allowlist: {name}"),
                ));
            }
            // Windows no distingue mayúsculas en variables de entorno.
            if !allowed.insert(name.to_ascii_uppercase()) {
                return Err(invalid(
                    "allowlist_duplicate",
                    format!("{name} repetido en allowlist"),
                ));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for binding in &env.variables {
            let name = binding.name();
            if !valid_env_name(name) {
                return Err(invalid(
                    "env_name_invalid",
                    format!("nombre de variable no válido: {name}"),
                ));
            }
            if !seen.insert(name.to_ascii_uppercase()) {
                return Err(invalid(
                    "env_duplicate",
                    format!("{name} definida dos veces (sin distinguir mayúsculas)"),
                ));
            }
            if !env.allowlist.iter().any(|a| a == name) {
                return Err(invalid(
                    "env_not_allowed",
                    format!("{name} no está en la allowlist del perfil"),
                ));
            }
            if let EnvironmentBinding::Credential {
                credential_reference,
                ..
            } = binding
            {
                if !credential_reference.starts_with("larm/") || credential_reference.len() < 6 {
                    return Err(invalid(
                        "credential_reference_invalid",
                        format!("{name}: la referencia debe estar bajo larm/"),
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Parámetros de lanzamiento (schema `LaunchParameters`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchParameters {
    pub threads: u32,
    #[serde(deserialize_with = "Option::deserialize")]
    pub context: Option<u32>,
    pub batch: u32,
    pub concurrency: u32,
    #[serde(deserialize_with = "Option::deserialize")]
    pub offload_layers: Option<u32>,
    pub backend_id: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub tokenizer_hash: Option<String>,
    #[serde(deserialize_with = "Option::deserialize")]
    pub chat_template_hash: Option<String>,
    pub generation: Generation,
    /// Límite adicional del perfil; `None` = sin límite adicional (nunca 0).
    #[serde(deserialize_with = "Option::deserialize")]
    pub memory_limit_bytes: Option<DecimalU64>,
    pub runtime_arguments: Vec<String>,
    pub environment: EnvironmentConfig,
    pub adapter_options: Map<String, Value>,
}

/// Motivo de rechazo de unos parámetros (antes de cualquier IO).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchInvalid {
    /// `memory_limit_zero`, `env_name_invalid`, `env_duplicate`,
    /// `env_not_allowed`, `credential_reference_invalid`, `generation_out_of_range`,
    /// `allowlist_duplicate`.
    pub reason: &'static str,
    pub detail: String,
}

fn invalid(reason: &'static str, detail: impl Into<String>) -> LaunchInvalid {
    LaunchInvalid {
        reason,
        detail: detail.into(),
    }
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl LaunchParameters {
    /// Reglas H03–H04 que no dependen del perfil cualificado del adaptador
    /// (los esquemas de argumentos y opciones los aplica el adaptador, P034).
    ///
    /// # Errors
    /// Primera regla violada.
    pub fn validate(&self) -> Result<(), LaunchInvalid> {
        if self.threads == 0 || self.context == Some(0) || self.batch == 0 || self.concurrency == 0
        {
            return Err(invalid(
                "launch_parameter_invalid",
                "threads, batch y concurrency deben ser ≥ 1; context debe ser ≥ 1 o null",
            ));
        }
        if !crate::bridge::is_uuid_v7(&self.backend_id) {
            return Err(invalid("backend_id_invalid", "backend_id debe ser UUIDv7"));
        }
        if self
            .tokenizer_hash
            .as_deref()
            .is_some_and(|hash| !valid_sha256(hash))
            || self
                .chat_template_hash
                .as_deref()
                .is_some_and(|hash| !valid_sha256(hash))
        {
            return Err(invalid(
                "launch_hash_invalid",
                "tokenizer_hash y chat_template_hash deben ser SHA-256 en minúsculas o null",
            ));
        }
        if self.memory_limit_bytes.is_some_and(|m| m.get() == 0) {
            return Err(invalid(
                "memory_limit_zero",
                "memory_limit_bytes debe ser positivo o null",
            ));
        }
        let g = &self.generation;
        if g.temperature.is_some_and(|t| !(0.0..=2.0).contains(&t))
            || g.top_p.is_some_and(|p| !(0.0..=1.0).contains(&p))
        {
            return Err(invalid(
                "generation_out_of_range",
                "temperature en 0..=2 y top_p en 0..=1",
            ));
        }
        if g.max_output_tokens == Some(0) {
            return Err(invalid(
                "generation_out_of_range",
                "max_output_tokens debe ser ≥ 1",
            ));
        }
        self.environment.validate()
    }

    /// `profile_hash`: sha256 del JSON RFC 8785 de los parámetros.
    ///
    /// # Errors
    /// Parámetros inválidos o no serializables de forma canónica.
    pub fn profile_hash(&self) -> Result<String, LaunchInvalid> {
        self.validate()?;
        let value =
            serde_json::to_value(self).map_err(|e| invalid("serialization", e.to_string()))?;
        crate::canonical::sha256_hex(&value).map_err(|e| invalid("serialization", e))
    }

    /// Valores de un perfil builtin: sin límite adicional, sin argumentos,
    /// entorno vacío y sin opciones (contrato 11 H03).
    #[must_use]
    pub fn builtin(
        backend_id: impl Into<String>,
        threads: u32,
        context: Option<u32>,
        batch: u32,
    ) -> Self {
        Self {
            threads,
            context,
            batch,
            concurrency: 1,
            offload_layers: None,
            backend_id: backend_id.into(),
            tokenizer_hash: None,
            chat_template_hash: None,
            generation: Generation {
                temperature: None,
                top_p: None,
                seed: None,
                max_output_tokens: None,
            },
            memory_limit_bytes: None,
            runtime_arguments: Vec::new(),
            environment: EnvironmentConfig::default(),
            adapter_options: Map::new(),
        }
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

/// Límite de memoria efectivo: el mínimo de los límites no nulos del perfil
/// y de `Constraints.max_memory_bytes`; `None` si ninguno limita.
#[must_use]
pub fn effective_memory_limit(
    profile: Option<DecimalU64>,
    constraint: Option<DecimalU64>,
) -> Option<DecimalU64> {
    match (profile, constraint) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    const BACKEND: &str = "0192aaaa-0000-7000-8000-000000000001";

    fn params() -> LaunchParameters {
        serde_json::from_value(json!({
            "threads": 16, "context": 8192, "batch": 512, "concurrency": 2, "offload_layers": 99,
            "backend_id": BACKEND, "tokenizer_hash": null, "chat_template_hash": null,
            "generation": {"temperature": 0.7, "top_p": 0.95, "seed": 42, "max_output_tokens": 512},
            "memory_limit_bytes": "34359738368",
            "runtime_arguments": ["--flash-attn", "--no-mmap"],
            "environment": {"allowlist": ["HSA_OVERRIDE_GFX_VERSION", "HF_TOKEN"], "variables": [
                {"name": "HSA_OVERRIDE_GFX_VERSION", "value": "11.5.1"},
                {"name": "HF_TOKEN", "credential_reference": "larm/source/hf"}]},
            "adapter_options": {"mlock": false, "rope": {"base": 10000, "scale": 1.0}}
        }))
        .unwrap()
    }

    #[test]
    fn mismo_input_mismo_hash_y_orden_de_propiedades_irrelevante() {
        let a = params().profile_hash().unwrap();
        assert_eq!(a.len(), 64);
        assert_eq!(params().profile_hash().unwrap(), a);
        let reordered: LaunchParameters = serde_json::from_str(
            r#"{"adapter_options":{"rope":{"scale":1.0,"base":10000},"mlock":false},"runtime_arguments":["--flash-attn","--no-mmap"],
            "environment":{"variables":[{"value":"11.5.1","name":"HSA_OVERRIDE_GFX_VERSION"},{"credential_reference":"larm/source/hf","name":"HF_TOKEN"}],
            "allowlist":["HSA_OVERRIDE_GFX_VERSION","HF_TOKEN"]},"memory_limit_bytes":"34359738368",
            "generation":{"max_output_tokens":512,"seed":42,"top_p":0.95,"temperature":0.7},"chat_template_hash":null,
            "tokenizer_hash":null,"backend_id":"0192aaaa-0000-7000-8000-000000000001","offload_layers":99,"concurrency":2,
            "batch":512,"context":8192,"threads":16}"#,
        )
        .unwrap();
        assert_eq!(reordered.profile_hash().unwrap(), a);
    }

    #[test]
    fn cambiar_solo_args_opciones_entorno_o_limite_cambia_el_hash() {
        let base = params().profile_hash().unwrap();
        type Edit = Box<dyn Fn(&mut LaunchParameters)>;
        let edits: Vec<(&str, Edit)> = vec![
            (
                "argumento",
                Box::new(|p| p.runtime_arguments.push("--verbose".into())),
            ),
            (
                "orden de argumentos",
                Box::new(|p| p.runtime_arguments.reverse()),
            ),
            (
                "opción",
                Box::new(|p| {
                    p.adapter_options.insert("mlock".into(), json!(true));
                }),
            ),
            (
                "valor de entorno",
                Box::new(|p| {
                    p.environment.variables[0] = EnvironmentBinding::Value {
                        name: "HSA_OVERRIDE_GFX_VERSION".into(),
                        value: "11.0.0".into(),
                    }
                }),
            ),
            (
                "referencia de credencial",
                Box::new(|p| {
                    p.environment.variables[1] = EnvironmentBinding::Credential {
                        name: "HF_TOKEN".into(),
                        credential_reference: "larm/source/hf2".into(),
                    }
                }),
            ),
            (
                "límite",
                Box::new(|p| p.memory_limit_bytes = DecimalU64::new(1 << 30)),
            ),
            ("sin límite", Box::new(|p| p.memory_limit_bytes = None)),
            (
                "temperatura",
                Box::new(|p| p.generation.temperature = Some(0.8)),
            ),
        ];
        for (what, edit) in edits {
            let mut p = params();
            edit(&mut p);
            assert_ne!(p.profile_hash().unwrap(), base, "{what}");
        }
    }

    #[test]
    fn el_hash_no_contiene_secretos_solo_referencias() {
        let canonical =
            crate::canonical::canonicalize(&serde_json::to_value(params()).unwrap()).unwrap();
        assert!(canonical.contains(r#""credential_reference":"larm/source/hf""#));
        assert!(canonical.contains(r#""temperature":0.7"#) && canonical.contains(r#""scale":1"#));
    }

    #[test]
    fn reglas_h03_h04() {
        let check = |edit: &dyn Fn(&mut LaunchParameters), reason: &str| {
            let mut p = params();
            edit(&mut p);
            assert_eq!(p.validate().unwrap_err().reason, reason);
            assert!(p.profile_hash().is_err());
        };
        check(
            &|p| p.memory_limit_bytes = DecimalU64::new(0),
            "memory_limit_zero",
        );
        check(&|p| p.threads = 0, "launch_parameter_invalid");
        check(&|p| p.context = Some(0), "launch_parameter_invalid");
        check(&|p| p.batch = 0, "launch_parameter_invalid");
        check(&|p| p.concurrency = 0, "launch_parameter_invalid");
        check(&|p| p.backend_id = "backend".into(), "backend_id_invalid");
        check(
            &|p| p.tokenizer_hash = Some("A".repeat(64)),
            "launch_hash_invalid",
        );
        check(
            &|p| p.environment.allowlist.push("1MAL".into()),
            "env_name_invalid",
        );
        check(
            &|p| p.environment.allowlist.push("hf_token".into()),
            "allowlist_duplicate",
        );
        check(
            &|p| {
                p.environment.variables.push(EnvironmentBinding::Value {
                    name: "hf_token".into(),
                    value: "x".into(),
                })
            },
            "env_duplicate",
        );
        check(
            &|p| {
                p.environment.allowlist.push("OTRA".into());
                p.environment.variables.push(EnvironmentBinding::Value {
                    name: "NO_LISTADA".into(),
                    value: "x".into(),
                });
            },
            "env_not_allowed",
        );
        check(
            &|p| {
                p.environment.variables[1] = EnvironmentBinding::Credential {
                    name: "HF_TOKEN".into(),
                    credential_reference: "otro/x".into(),
                }
            },
            "credential_reference_invalid",
        );
        check(
            &|p| p.generation.top_p = Some(1.5),
            "generation_out_of_range",
        );
        // Un binding con value y credential_reference a la vez no es del schema.
        let both = json!({"name": "A", "value": "x", "credential_reference": "larm/a"});
        assert!(serde_json::from_value::<EnvironmentBinding>(both).is_err());
        // memory_limit "0" o número JSON se rechazan al leer.
        let mut v = serde_json::to_value(params()).unwrap();
        v["memory_limit_bytes"] = json!(34_359_738_368_u64);
        assert!(serde_json::from_value::<LaunchParameters>(v).is_err());
    }

    #[test]
    fn nullable_obligatorios_del_perfil_no_se_omiten() {
        let mut value = serde_json::to_value(params()).unwrap();
        value.as_object_mut().unwrap().remove("context");
        assert!(serde_json::from_value::<LaunchParameters>(value).is_err());

        let mut value = serde_json::to_value(params()).unwrap();
        value["generation"].as_object_mut().unwrap().remove("seed");
        assert!(serde_json::from_value::<LaunchParameters>(value).is_err());
    }

    #[test]
    fn builtin_y_limite_efectivo() {
        let b = LaunchParameters::builtin(BACKEND, 8, None, 256);
        let v = serde_json::to_value(&b).unwrap();
        assert_eq!(v["memory_limit_bytes"], Value::Null);
        assert_eq!(v["runtime_arguments"], json!([]));
        assert_eq!(v["environment"], json!({"allowlist": [], "variables": []}));
        assert_eq!(v["adapter_options"], json!({}));
        assert!(b.profile_hash().is_ok());
        let g = |n: u64| DecimalU64::new(n);
        assert_eq!(effective_memory_limit(g(10), g(4)), g(4));
        assert_eq!(effective_memory_limit(None, g(4)), g(4));
        assert_eq!(effective_memory_limit(g(10), None), g(10));
        assert_eq!(effective_memory_limit(None, None), None);
    }
}
