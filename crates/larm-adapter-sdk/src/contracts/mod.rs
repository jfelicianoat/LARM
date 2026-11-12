//! Validación de las 40 firmas desde schemas generados; sin IO ni resolución externa.
use std::collections::BTreeMap;
use std::sync::OnceLock;

use jsonschema::{Keyword, ValidationError, Validator};
use larm_domain::error::{ErrorCode, LarmError};
use serde_json::Value;

mod generated;

struct MaximumDecimal(u64);
impl<'i> Keyword<'i> for MaximumDecimal {
    fn validate(&self, instance: &'i Value) -> Result<(), ValidationError<'i>> {
        if self.is_valid(instance) {
            Ok(())
        } else {
            Err(ValidationError::custom(
                "Decimal fuera del límite publicado",
            ))
        }
    }
    fn is_valid(&self, instance: &'i Value) -> bool {
        instance
            .as_str()
            .is_none_or(|value| value.parse::<u64>().is_ok_and(|value| value <= self.0))
    }
}

pub struct Contracts {
    methods: BTreeMap<&'static str, (Validator, Validator)>,
}

fn invalid(code: ErrorCode, reason: &str) -> LarmError {
    // El error no contiene el valor rechazado, paths del schema ni metadatos.
    LarmError::new(code, "Documento del adaptador fuera del contrato", reason)
}

impl Contracts {
    pub fn build() -> Result<Self, LarmError> {
        let mut methods = BTreeMap::new();
        for (method, input, output) in generated::SCHEMAS {
            let pair = (Self::compile(input)?, Self::compile(output)?);
            if methods.insert(*method, pair).is_some() {
                return Err(invalid(
                    ErrorCode::ProtocolError,
                    "adapter_contract_duplicate",
                ));
            }
        }
        let table = crate::manifest::MethodTable::contract();
        if table.methods.len() != methods.len()
            || table
                .methods
                .iter()
                .any(|entry| !methods.contains_key(entry.method.as_str()))
        {
            return Err(invalid(
                ErrorCode::ProtocolError,
                "adapter_contract_table_mismatch",
            ));
        }
        Ok(Self { methods })
    }

    pub fn shared() -> Result<&'static Self, LarmError> {
        static CONTRACTS: OnceLock<Result<Contracts, LarmError>> = OnceLock::new();
        CONTRACTS
            .get_or_init(Self::build)
            .as_ref()
            .map_err(Clone::clone)
    }

    pub(crate) fn compile(raw: &str) -> Result<Validator, LarmError> {
        let schema: Value = serde_json::from_str(raw)
            .map_err(|_| invalid(ErrorCode::ProtocolError, "adapter_contract_invalid"))?;
        // Sólo fragmentos locales; incluso un schema alterado no abre red/archivos.
        Self::local_refs(&schema)?;
        jsonschema::draft202012::options()
            .with_keyword("x-maximum-decimal", |_, value: &Value, _| {
                let limit = value
                    .as_str()
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or_else(|| {
                        ValidationError::custom("Límite decimal del contrato inválido")
                    })?;
                Ok(Box::new(MaximumDecimal(limit)) as Box<dyn for<'i> Keyword<'i>>)
            })
            .should_validate_formats(true)
            .should_ignore_unknown_formats(false)
            .build(&schema)
            .map_err(|_| invalid(ErrorCode::ProtocolError, "adapter_contract_invalid"))
    }

    fn local_refs(value: &Value) -> Result<(), LarmError> {
        match value {
            Value::Array(values) => {
                for child in values {
                    Self::local_refs(child)?;
                }
            }
            Value::Object(values) => {
                if values.keys().any(|k| {
                    matches!(
                        k.as_str(),
                        "$id" | "$anchor" | "$dynamicRef" | "$dynamicAnchor" | "$recursiveRef"
                    )
                }) || values
                    .get("$ref")
                    .is_some_and(|v| !v.as_str().is_some_and(|s| s.starts_with("#/$defs/")))
                {
                    return Err(invalid(
                        ErrorCode::ProtocolError,
                        "adapter_contract_external_ref",
                    ));
                }
                for child in values.values() {
                    Self::local_refs(child)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn request(&self, method: &str, value: &Value) -> Result<(), LarmError> {
        self.validate(method, value, false)
    }
    pub fn response(&self, method: &str, value: &Value) -> Result<(), LarmError> {
        self.validate(method, value, true)
    }
    fn validate(&self, method: &str, value: &Value, output: bool) -> Result<(), LarmError> {
        let (input, reply) = self
            .methods
            .get(method)
            .ok_or_else(|| invalid(ErrorCode::UnsupportedCapability, "method_not_announced"))?;
        if (if output { reply } else { input }).is_valid(value) {
            return Ok(());
        }
        Err(if output {
            invalid(ErrorCode::ProtocolError, "invalid_adapter_reply")
        } else {
            invalid(ErrorCode::ValidationFailed, "adapter_request_invalid")
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn externos_y_scopes_no_se_resuelven() {
        for schema in [
            serde_json::json!({"$ref":"https://127.0.0.1:1/never"}),
            serde_json::json!({"$ref":"file:///C:/never"}),
            serde_json::json!({"$dynamicRef":"#x"}),
            serde_json::json!({"$id":"https://example.invalid/","type":"object"}),
        ] {
            assert_eq!(
                Contracts::compile(&schema.to_string())
                    .unwrap_err()
                    .details
                    .reason,
                "adapter_contract_external_ref"
            );
        }
    }
}
