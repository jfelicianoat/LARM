#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeMap;

use larm_domain::custom_rules::{
    CustomRule, CustomRules, CustomRulesError, RuleContext, RuleDisposition, RuleEffect, RuleField,
    RuleOperator, RulePredicate,
};
use serde_json::{json, Value};

fn predicate(field: RuleField, operator: RuleOperator, value: Value) -> RulePredicate {
    RulePredicate {
        field,
        operator,
        value,
    }
}

fn rule(
    id: &str,
    predicates: Vec<RulePredicate>,
    effect: RuleEffect,
    limit_bytes: Option<&str>,
    target: Option<&str>,
) -> CustomRule {
    CustomRule {
        id: id.into(),
        predicates,
        effect,
        limit_bytes: limit_bytes.map(str::to_string),
        target_storage_location_id: target.map(str::to_string),
    }
}

fn rules(rules: Vec<CustomRule>) -> CustomRules {
    CustomRules {
        schema_version: "1.0".into(),
        rules,
    }
}

fn context() -> RuleContext {
    RuleContext::new(BTreeMap::from([
        (RuleField::Format, json!("gguf")),
        (
            RuleField::Capability,
            json!(["text.generate", "text.embed"]),
        ),
        (RuleField::OsBuild, json!(26200)),
    ]))
}

const LOCATION_A: &str = "018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a101";
const LOCATION_B: &str = "018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a102";

#[test]
fn serializa_el_schema_cerrado_y_exige_nullable_explicito() {
    let value = json!({
        "schema_version": "1.0",
        "rules": [{
            "id": "gguf-only",
            "predicates": [{"field": "format", "operator": "eq", "value": "gguf"}],
            "effect": "require",
            "limit_bytes": null,
            "target_storage_location_id": LOCATION_A
        }]
    });
    let parsed: CustomRules = serde_json::from_value(value.clone()).unwrap();
    parsed.validate().unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);

    let mut missing = value;
    missing["rules"][0]
        .as_object_mut()
        .unwrap()
        .remove("limit_bytes");
    assert!(serde_json::from_value::<CustomRules>(missing).is_err());
    let extra = json!({"schema_version":"1.0","rules":[],"script":"run arbitrary code"});
    assert!(serde_json::from_value::<CustomRules>(extra).is_err());
    assert!(serde_json::from_value::<RulePredicate>(json!({
        "field": "path", "operator": "eq", "value": "C:\\"
    }))
    .is_err());
    assert!(serde_json::from_value::<RulePredicate>(json!({
        "field": "format", "operator": "regex", "value": ".*"
    }))
    .is_err());
}

#[test]
fn aplica_operadores_declarativos_y_and_en_las_predicates() {
    let doc = rules(vec![rule(
        "supported-gguf",
        vec![
            predicate(RuleField::Format, RuleOperator::Eq, json!("gguf")),
            predicate(
                RuleField::Capability,
                RuleOperator::Contains,
                json!("text.generate"),
            ),
            predicate(
                RuleField::OsBuild,
                RuleOperator::Range,
                json!([25000, 27000]),
            ),
            predicate(RuleField::Quantization, RuleOperator::Exists, json!(false)),
        ],
        RuleEffect::Limit,
        Some("1000"),
        None,
    )]);
    doc.validate().unwrap();
    let preview = doc.preview(&context(), None).unwrap();
    assert_eq!(preview.disposition, RuleDisposition::Limit);
    assert_eq!(preview.effective_limit_bytes, Some(1000));
    assert_eq!(preview.matched_rule_ids, ["supported-gguf"]);

    let mut facts = context();
    facts.insert(RuleField::Format, json!("safetensors"));
    assert_eq!(
        doc.preview(&facts, None).unwrap().disposition,
        RuleDisposition::NoMatch,
        "una predicate falsa impide que se active la regla completa"
    );

    let require = rules(vec![rule(
        "requires-gguf",
        vec![predicate(
            RuleField::Format,
            RuleOperator::Eq,
            json!("gguf"),
        )],
        RuleEffect::Require,
        None,
        None,
    )]);
    let missing_requirement = require.preview(&facts, None).unwrap();
    assert_eq!(
        missing_requirement.disposition,
        RuleDisposition::RequirementNotMet
    );
    assert_eq!(missing_requirement.blocking_rule_ids, ["requires-gguf"]);
}

#[test]
fn operadores_aceptan_valores_array_del_schema() {
    let capabilities = json!(["text.generate", "text.embed"]);
    let doc = rules(vec![
        rule(
            "capabilities-equal",
            vec![predicate(
                RuleField::Capability,
                RuleOperator::Eq,
                capabilities.clone(),
            )],
            RuleEffect::Limit,
            Some("900"),
            None,
        ),
        rule(
            "capabilities-contain",
            vec![predicate(
                RuleField::Capability,
                RuleOperator::Contains,
                capabilities,
            )],
            RuleEffect::Limit,
            Some("300"),
            None,
        ),
        rule(
            "empty-membership-set",
            vec![predicate(RuleField::Format, RuleOperator::In, json!([]))],
            RuleEffect::Deny,
            None,
            None,
        ),
    ]);
    doc.validate().unwrap();
    let preview = doc.preview(&context(), None).unwrap();
    assert_eq!(preview.disposition, RuleDisposition::Limit);
    assert_eq!(preview.effective_limit_bytes, Some(300));
    assert_eq!(
        preview.matched_rule_ids,
        ["capabilities-contain", "capabilities-equal"]
    );
}

#[test]
fn precedencia_deny_require_limit_y_limite_minimo() {
    let format = predicate(RuleField::Format, RuleOperator::Eq, json!("gguf"));
    let doc = rules(vec![
        rule(
            "large-cap",
            vec![format.clone()],
            RuleEffect::Limit,
            Some("900"),
            None,
        ),
        rule(
            "small-cap",
            vec![format.clone()],
            RuleEffect::Limit,
            Some("300"),
            None,
        ),
        rule(
            "use-primary",
            vec![format.clone()],
            RuleEffect::Require,
            None,
            Some(LOCATION_A),
        ),
        rule(
            "deny-primary",
            vec![format],
            RuleEffect::Deny,
            None,
            Some(LOCATION_B),
        ),
    ]);
    doc.validate().unwrap();

    let preview = doc.preview(&context(), Some(LOCATION_B)).unwrap();
    assert_eq!(preview.disposition, RuleDisposition::Deny);
    assert_eq!(preview.blocking_rule_ids, ["deny-primary"]);
    assert_eq!(preview.effective_limit_bytes, Some(300));

    let required = doc.preview(&context(), Some(LOCATION_A)).unwrap();
    assert_eq!(required.disposition, RuleDisposition::Require);
    assert_eq!(
        required.required_storage_location_id.as_deref(),
        Some(LOCATION_A)
    );
    assert_eq!(required.effective_limit_bytes, Some(300));

    let elsewhere = doc
        .preview(&context(), Some("018f1f7e-7b2c-7cc0-8b0e-9a0c69f3a103"))
        .unwrap();
    assert_eq!(elsewhere.disposition, RuleDisposition::RequirementNotMet);
    assert_eq!(elsewhere.blocking_rule_ids, ["use-primary"]);
}

#[test]
fn rechaza_values_operadores_limites_targets_y_contradicciones() {
    let invalid = [
        rule(
            "in-needs-array",
            vec![predicate(
                RuleField::Format,
                RuleOperator::In,
                json!("gguf"),
            )],
            RuleEffect::Deny,
            None,
            None,
        ),
        rule(
            "in-values-match-schema",
            vec![predicate(
                RuleField::Format,
                RuleOperator::In,
                json!([true]),
            )],
            RuleEffect::Deny,
            None,
            None,
        ),
        rule(
            "backwards-range",
            vec![predicate(
                RuleField::OsBuild,
                RuleOperator::Range,
                json!([9, 1]),
            )],
            RuleEffect::Deny,
            None,
            None,
        ),
        rule(
            "leading-zero-limit",
            vec![predicate(
                RuleField::Format,
                RuleOperator::Eq,
                json!("gguf"),
            )],
            RuleEffect::Limit,
            Some("0300"),
            None,
        ),
    ];
    for bad in invalid {
        assert!(rules(vec![bad]).validate().is_err());
    }
    let duplicate_id = rules(vec![
        rule(
            "same-id",
            vec![predicate(
                RuleField::Format,
                RuleOperator::Eq,
                json!("gguf"),
            )],
            RuleEffect::Deny,
            None,
            None,
        ),
        rule(
            "same-id",
            vec![predicate(
                RuleField::Format,
                RuleOperator::Eq,
                json!("safetensors"),
            )],
            RuleEffect::Deny,
            None,
            None,
        ),
    ]);
    assert!(duplicate_id.validate().is_err());
    let mut bad_version = rules(Vec::new());
    bad_version.schema_version = "2.0".into();
    assert_eq!(
        bad_version.validate(),
        Err(CustomRulesError::UnsupportedSchemaVersion)
    );
    assert!(rules(vec![rule(
        "too-large",
        vec![predicate(
            RuleField::Format,
            RuleOperator::Eq,
            json!("gguf")
        )],
        RuleEffect::Limit,
        Some("9223372036854775808"),
        None,
    )])
    .validate()
    .is_err());
    assert!(rules(vec![rule(
        "bad-target",
        vec![predicate(
            RuleField::Format,
            RuleOperator::Eq,
            json!("gguf")
        )],
        RuleEffect::Require,
        None,
        Some("C:\\models"),
    )])
    .validate()
    .is_err());

    let predicate = predicate(RuleField::Format, RuleOperator::Eq, json!("gguf"));
    let conflict = rules(vec![
        rule(
            "allow",
            vec![predicate.clone()],
            RuleEffect::Require,
            None,
            Some(LOCATION_A),
        ),
        rule(
            "block",
            vec![predicate],
            RuleEffect::Deny,
            None,
            Some(LOCATION_A),
        ),
    ]);
    assert!(matches!(
        conflict.validate(),
        Err(CustomRulesError::ContradictoryRules(_, _))
    ));
}

#[test]
fn evaluator_rechaza_require_targets_incompatibles_que_coinciden() {
    let doc = rules(vec![
        rule(
            "primary",
            vec![predicate(
                RuleField::Format,
                RuleOperator::Eq,
                json!("gguf"),
            )],
            RuleEffect::Require,
            None,
            Some(LOCATION_A),
        ),
        rule(
            "secondary",
            vec![predicate(
                RuleField::OsBuild,
                RuleOperator::Range,
                json!([26000, 27000]),
            )],
            RuleEffect::Require,
            None,
            Some(LOCATION_B),
        ),
    ]);
    assert_eq!(doc.validate(), Ok(()));
    assert_eq!(
        doc.preview(&context(), None),
        Err(CustomRulesError::ConflictingRequireTargets)
    );
}
