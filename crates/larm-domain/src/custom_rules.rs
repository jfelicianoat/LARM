//! CUSTOM_RULES declarativas para auto-placement (P027; contratos 02/03).
//!
//! Las reglas son datos JSON cerrados: no contienen expresiones ni código. La
//! validación semántica complementa el schema público y `preview` ofrece una
//! evaluación determinista para el editor de reglas.

use std::collections::{BTreeMap, BTreeSet};

use crate::ports::MutationContext;
use async_trait::async_trait;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use thiserror::Error;

const MAX_LIMIT_BYTES: u64 = i64::MAX as u64;

/// Documento versionado guardado por `/custom-rules`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomRules {
    pub schema_version: String,
    pub rules: Vec<CustomRule>,
}

impl Default for CustomRules {
    fn default() -> Self {
        Self {
            schema_version: "1.0".into(),
            rules: Vec::new(),
        }
    }
}

/// Revisión persistida de la configuración. La revisión viaja por ETag para
/// conservar el DTO público `CustomRules` sin campos de transporte.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VersionedCustomRules {
    pub value: CustomRules,
    pub revision: i64,
}

/// Persistencia versionada de las reglas de auto-placement.
#[async_trait]
pub trait CustomRulesRepository: Send + Sync {
    async fn get(&self) -> Result<VersionedCustomRules, crate::error::LarmError>;
    async fn replace(
        &self,
        expected_revision: i64,
        value: &CustomRules,
        ctx: &MutationContext,
    ) -> Result<VersionedCustomRules, crate::error::LarmError>;
}

/// Regla cerrada: todas sus predicates se combinan con AND.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomRule {
    pub id: String,
    pub predicates: Vec<RulePredicate>,
    pub effect: RuleEffect,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub limit_bytes: Option<String>,
    #[serde(deserialize_with = "deserialize_required_option")]
    pub target_storage_location_id: Option<String>,
}

fn deserialize_required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::deserialize(deserializer)
}

/// Campos admitidos por el contrato 03. El enum impide que una regla consulte
/// rutas, secretos u otros datos no autorizados.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleField {
    Format,
    Architecture,
    Quantization,
    Capability,
    Backend,
    OsBuild,
}

/// Operaciones puras y acotadas del DSL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleOperator {
    Eq,
    In,
    Contains,
    Range,
    Exists,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulePredicate {
    pub field: RuleField,
    pub operator: RuleOperator,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleEffect {
    Require,
    Deny,
    Limit,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CustomRulesError {
    #[error("schema_version debe ser 1.0")]
    UnsupportedSchemaVersion,
    #[error("id de regla vacío o duplicado: {0}")]
    InvalidRuleId(String),
    #[error("la regla {0} necesita al menos una predicate")]
    EmptyPredicates(String),
    #[error("predicate inválida en la regla {0}")]
    InvalidPredicate(String),
    #[error("la regla {0} tiene un target_storage_location_id inválido")]
    InvalidTarget(String),
    #[error("limit_bytes inválido en la regla {0}")]
    InvalidLimit(String),
    #[error("limit_bytes sólo se permite con effect=limit en la regla {0}")]
    UnexpectedLimit(String),
    #[error("reglas contradictorias: {0} y {1}")]
    ContradictoryRules(String, String),
    #[error("varias reglas require activas exigen ubicaciones distintas")]
    ConflictingRequireTargets,
    #[error("candidate_storage_location_id inválido")]
    InvalidCandidateTarget,
}

/// Contexto observable por las predicates. Los valores son los facts del
/// catálogo y nunca se interpretan como código.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleContext {
    fields: BTreeMap<RuleField, Value>,
}

impl RuleContext {
    #[must_use]
    pub fn new(fields: BTreeMap<RuleField, Value>) -> Self {
        Self { fields }
    }

    pub fn insert(&mut self, field: RuleField, value: Value) -> Option<Value> {
        self.fields.insert(field, value)
    }

    #[must_use]
    pub fn get(&self, field: RuleField) -> Option<&Value> {
        self.fields.get(&field)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleDisposition {
    Deny,
    RequirementNotMet,
    Require,
    Limit,
    NoMatch,
}

/// Resultado de preview apto para presentar en la GUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomRulesPreview {
    pub disposition: RuleDisposition,
    pub matched_rule_ids: Vec<String>,
    pub blocking_rule_ids: Vec<String>,
    pub required_storage_location_id: Option<String>,
    pub effective_limit_bytes: Option<u64>,
}

impl CustomRules {
    /// Valida reglas y combinaciones que no puede expresar el JSON Schema.
    ///
    /// # Errors
    /// Rechaza valores fuera del DSL, límites incoherentes y contradicciones.
    pub fn validate(&self) -> Result<(), CustomRulesError> {
        if self.schema_version != "1.0" {
            return Err(CustomRulesError::UnsupportedSchemaVersion);
        }

        let mut ids = BTreeSet::new();
        for rule in &self.rules {
            if rule.id.is_empty() || !ids.insert(rule.id.as_str()) {
                return Err(CustomRulesError::InvalidRuleId(rule.id.clone()));
            }
            if rule.predicates.is_empty() {
                return Err(CustomRulesError::EmptyPredicates(rule.id.clone()));
            }
            if rule
                .target_storage_location_id
                .as_deref()
                .is_some_and(|id| !is_uuid_v7(id))
            {
                return Err(CustomRulesError::InvalidTarget(rule.id.clone()));
            }
            match (rule.effect, rule.limit_bytes.as_deref()) {
                (RuleEffect::Limit, Some(bytes)) if parse_limit(bytes).is_some() => {}
                (RuleEffect::Limit, _) => {
                    return Err(CustomRulesError::InvalidLimit(rule.id.clone()));
                }
                (_, Some(_)) => {
                    return Err(CustomRulesError::UnexpectedLimit(rule.id.clone()));
                }
                (_, None) => {}
            }

            for predicate in &rule.predicates {
                if !valid_predicate_value(predicate) {
                    return Err(CustomRulesError::InvalidPredicate(rule.id.clone()));
                }
            }
        }

        for (index, left) in self.rules.iter().enumerate() {
            let left_key = rule_predicate_key(left);
            for right in &self.rules[index + 1..] {
                let same_predicates = left_key == rule_predicate_key(right);
                let deny_require_overlap = matches!(
                    (left.effect, right.effect),
                    (RuleEffect::Deny, RuleEffect::Require)
                        | (RuleEffect::Require, RuleEffect::Deny)
                ) && (left.target_storage_location_id.is_none()
                    || right.target_storage_location_id.is_none()
                    || left.target_storage_location_id == right.target_storage_location_id);
                let conflicting_require_targets = left.effect == RuleEffect::Require
                    && right.effect == RuleEffect::Require
                    && left.target_storage_location_id != right.target_storage_location_id;
                if same_predicates && (deny_require_overlap || conflicting_require_targets) {
                    return Err(CustomRulesError::ContradictoryRules(
                        left.id.clone(),
                        right.id.clone(),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Evalúa la vista previa para una solicitud y, opcionalmente, una
    /// ubicación concreta. Precedencia: deny, require, limit.
    ///
    /// # Errors
    /// Devuelve el mismo error de `validate` o si varias reglas activas
    /// requieren ubicaciones incompatibles.
    pub fn preview(
        &self,
        context: &RuleContext,
        candidate_storage_location_id: Option<&str>,
    ) -> Result<CustomRulesPreview, CustomRulesError> {
        self.validate()?;
        if candidate_storage_location_id.is_some_and(|id| !is_uuid_v7(id)) {
            return Err(CustomRulesError::InvalidCandidateTarget);
        }

        let mut matched_rule_ids = Vec::new();
        let mut denials = Vec::new();
        let mut unmet_requirements = Vec::new();
        let mut requirements = Vec::new();
        let mut limits = Vec::new();
        let mut required_targets = BTreeSet::new();
        let candidate_matches = |target: &Option<String>| {
            target
                .as_deref()
                .is_none_or(|target| candidate_storage_location_id == Some(target))
        };

        for rule in &self.rules {
            let predicates_match = rule
                .predicates
                .iter()
                .all(|predicate| predicate_matches(predicate, context));
            match rule.effect {
                RuleEffect::Require if predicates_match => {
                    if let Some(target) = &rule.target_storage_location_id {
                        required_targets.insert(target.clone());
                    }
                    if !candidate_matches(&rule.target_storage_location_id) {
                        unmet_requirements.push(rule.id.clone());
                    } else {
                        matched_rule_ids.push(rule.id.clone());
                        requirements.push(rule.id.clone());
                    }
                }
                RuleEffect::Deny
                    if predicates_match && candidate_matches(&rule.target_storage_location_id) =>
                {
                    matched_rule_ids.push(rule.id.clone());
                    denials.push(rule.id.clone());
                }
                RuleEffect::Limit
                    if predicates_match && candidate_matches(&rule.target_storage_location_id) =>
                {
                    matched_rule_ids.push(rule.id.clone());
                    limits.push((
                        rule.id.clone(),
                        parse_limit(rule.limit_bytes.as_deref().unwrap_or_default()).unwrap_or(0),
                    ));
                }
                _ => {
                    if rule.effect == RuleEffect::Require && !predicates_match {
                        unmet_requirements.push(rule.id.clone());
                    }
                }
            }
        }

        if denials.is_empty() && required_targets.len() > 1 {
            return Err(CustomRulesError::ConflictingRequireTargets);
        }
        let required_storage_location_id = required_targets.into_iter().next();
        let effective_limit_bytes = limits.iter().map(|(_, bytes)| *bytes).min();
        matched_rule_ids.sort();
        denials.sort();
        unmet_requirements.sort();
        requirements.sort();
        limits.sort_by(|a, b| a.0.cmp(&b.0));

        let (disposition, blocking_rule_ids) = if !denials.is_empty() {
            (RuleDisposition::Deny, denials)
        } else if !unmet_requirements.is_empty() {
            (RuleDisposition::RequirementNotMet, unmet_requirements)
        } else if !requirements.is_empty() {
            (RuleDisposition::Require, Vec::new())
        } else if !limits.is_empty() {
            (RuleDisposition::Limit, Vec::new())
        } else {
            (RuleDisposition::NoMatch, Vec::new())
        };

        Ok(CustomRulesPreview {
            disposition,
            matched_rule_ids,
            blocking_rule_ids,
            required_storage_location_id,
            effective_limit_bytes,
        })
    }
}

fn valid_predicate_value(predicate: &RulePredicate) -> bool {
    match predicate.operator {
        RuleOperator::Eq | RuleOperator::Contains => is_schema_predicate_value(&predicate.value),
        RuleOperator::In => predicate
            .value
            .as_array()
            .is_some_and(|values| values.iter().all(is_string_or_nonnegative_integer)),
        RuleOperator::Range => predicate.value.as_array().is_some_and(|values| {
            if values.len() != 2 {
                return false;
            }
            let Some(min) = nonnegative_integer(&values[0]) else {
                return false;
            };
            let Some(max) = nonnegative_integer(&values[1]) else {
                return false;
            };
            min <= max
        }),
        RuleOperator::Exists => predicate.value.is_boolean(),
    }
}

fn is_scalar(value: &Value) -> bool {
    is_string_or_nonnegative_integer(value) || value.is_boolean()
}

fn is_schema_predicate_value(value: &Value) -> bool {
    is_scalar(value)
        || value
            .as_array()
            .is_some_and(|values| values.iter().all(is_string_or_nonnegative_integer))
}

fn is_string_or_nonnegative_integer(value: &Value) -> bool {
    value.as_str().is_some_and(|text| !text.is_empty()) || nonnegative_integer(value).is_some()
}

fn nonnegative_integer(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
}

fn parse_limit(value: &str) -> Option<u64> {
    if value.is_empty()
        || value.len() > 19
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let bytes = value.parse::<u64>().ok()?;
    (bytes <= MAX_LIMIT_BYTES).then_some(bytes)
}

fn is_uuid_v7(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36
        || [8, 13, 18, 23]
            .into_iter()
            .any(|index| bytes[index] != b'-')
        || bytes.iter().enumerate().any(|(index, byte)| {
            ![8, 13, 18, 23].contains(&index)
                && !byte.is_ascii_digit()
                && !(b'a'..=b'f').contains(byte)
        })
    {
        return false;
    }
    bytes[14] == b'7' && matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
}

fn predicate_matches(predicate: &RulePredicate, context: &RuleContext) -> bool {
    let actual = context.get(predicate.field);
    match predicate.operator {
        RuleOperator::Exists => actual.is_some() == predicate.value.as_bool().unwrap_or(false),
        RuleOperator::Eq => actual.is_some_and(|value| value == &predicate.value),
        RuleOperator::In => {
            let Some(allowed) = predicate.value.as_array() else {
                return false;
            };
            actual.is_some_and(|value| match value {
                Value::Array(values) => values.iter().any(|item| allowed.contains(item)),
                scalar => allowed.contains(scalar),
            })
        }
        RuleOperator::Contains => {
            if let Some(needles) = predicate.value.as_array() {
                return actual.is_some_and(|value| {
                    value
                        .as_array()
                        .is_some_and(|values| needles.iter().all(|needle| values.contains(needle)))
                });
            }
            let Some(needle) = predicate.value.as_str() else {
                return actual.is_some_and(|value| {
                    value
                        .as_array()
                        .is_some_and(|values| values.contains(&predicate.value))
                });
            };
            actual.is_some_and(|value| {
                value.as_str().is_some_and(|text| text.contains(needle))
                    || value
                        .as_array()
                        .is_some_and(|values| values.contains(&predicate.value))
            })
        }
        RuleOperator::Range => {
            let Some(bounds) = predicate.value.as_array() else {
                return false;
            };
            let Some(min) = bounds.first().and_then(nonnegative_integer) else {
                return false;
            };
            let Some(max) = bounds.get(1).and_then(nonnegative_integer) else {
                return false;
            };
            actual
                .and_then(nonnegative_integer)
                .is_some_and(|number| (min..=max).contains(&number))
        }
    }
}

fn predicate_key(predicate: &RulePredicate) -> String {
    format!(
        "{}:{}:{}",
        serde_json::to_string(&predicate.field).unwrap_or_default(),
        serde_json::to_string(&predicate.operator).unwrap_or_default(),
        predicate.value
    )
}

fn rule_predicate_key(rule: &CustomRule) -> Vec<String> {
    let mut predicates = rule
        .predicates
        .iter()
        .map(predicate_key)
        .collect::<Vec<_>>();
    predicates.sort();
    predicates
}
