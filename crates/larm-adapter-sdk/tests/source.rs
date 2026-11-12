//! Conducta de la frontera H05: lo que no viaja upstream sigue pendiente en Core.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;

use larm_adapter_sdk::source::{SearchSupport, SourceSearch, CAPABILITY_FILTER, TEXT_FILTER};
use larm_domain::error::ErrorCode;
use larm_domain::search::Search;
use serde_json::{json, Value};

const SOURCE: &str = "0192aaaa-0000-7000-8000-000000000001";

fn query() -> Value {
    json!({
        "q": "llama", "capabilities": ["text.generate", "embedding.create"],
        "source_ids": [SOURCE], "compatibility": "COMPATIBLE",
        "architectures": ["llama"], "formats": ["GGUF", "ONNX"],
        "quantizations": ["Q4_K_M"], "licenses": ["apache-2.0"],
        "runtime_ids": ["0192aaaa-0000-7000-8000-000000000002"],
        "parameter_count": {"minimum": "9007199254740993", "maximum": "9223372036854775807"},
        "size_bytes": {"minimum": "0", "maximum": "20000000000"},
        "trust_score": {"minimum": 50, "maximum": 100},
        "certification_statuses": ["CERTIFIED"], "only_executable": true,
        "only_recommended": true, "show_incompatible": false,
        "cursor": "federated-signed-cursor", "limit": 50
    })
}

fn full() -> Search {
    serde_json::from_value(query()).unwrap()
}

fn support(filters: &[&str]) -> SearchSupport {
    SearchSupport {
        search_applicable: true,
        supported_filters: filters.iter().map(|s| s.to_string()).collect(),
        limitations: vec!["upstream_is_partial".into()],
    }
}

#[test]
fn source_search_cerrado_claves_obligatorias_y_limites() {
    let value =
        json!({"source_id": SOURCE, "q": "", "capabilities": [], "cursor": null, "limit": 1});
    let parsed: SourceSearch = serde_json::from_value(value.clone()).unwrap();
    parsed.validate().unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
    for field in value.as_object().unwrap().keys() {
        let mut missing = value.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<SourceSearch>(missing).is_err(),
            "{field}"
        );
    }
    for field in [
        "architectures",
        "only_executable",
        "filterdigest",
        "unknown",
    ] {
        let mut extra = value.clone();
        extra[field] = json!(true);
        assert!(serde_json::from_value::<SourceSearch>(extra).is_err());
    }
    for (field, bad) in [
        ("source_id", json!("0192aaaa-0000-4000-8000-000000000001")),
        ("q", json!("á".repeat(501))),
        ("cursor", json!("")),
        ("limit", json!(0)),
        ("limit", json!(201)),
    ] {
        let mut invalid = value.clone();
        invalid[field] = bad;
        let request: SourceSearch = serde_json::from_value(invalid).unwrap();
        assert_eq!(
            request.validate().unwrap_err().details.resource_ids,
            [field]
        );
    }
    let mut unicode: SourceSearch = serde_json::from_value(value).unwrap();
    unicode.q = "á".repeat(500);
    unicode.limit = 200;
    unicode.validate().unwrap();
}

#[test]
fn proyeccion_retiene_todos_los_filtros_y_no_envia_cursor_federado() {
    let query = full();
    let support = support(&[TEXT_FILTER, CAPABILITY_FILTER]);
    let projection = support
        .project(&query, SOURCE, Some("upstream-page-2".into()))
        .unwrap();
    assert_eq!(
        projection.full, query,
        "incluidos strings decimales > 2^53, límites, toggles y cursor"
    );
    assert_eq!(projection.upstream.q, "llama");
    assert_eq!(
        projection.upstream.capabilities,
        ["text.generate", "embedding.create"]
    );
    assert_eq!(projection.upstream.source_id, SOURCE);
    assert_eq!(
        projection.upstream.cursor.as_deref(),
        Some("upstream-page-2")
    );
    assert_eq!(projection.upstream.limit, 50);
    assert_eq!(
        projection.deferred_filters,
        BTreeSet::from([
            "source_ids",
            "architectures",
            "formats",
            "quantizations",
            "runtime_ids",
            "licenses",
            "parameter_count",
            "size_bytes",
            "trust_score",
            "certification_statuses",
            "compatibility",
            "only_executable",
            "only_recommended",
            "show_incompatible"
        ])
    );
    assert_eq!(projection.limitations, ["upstream_is_partial"]);
    assert!(!serde_json::to_string(&projection.upstream)
        .unwrap()
        .contains("federated-signed-cursor"));
    assert_eq!(projection.filter_digest, query.filter_digest().unwrap());
}

#[test]
fn filtro_no_soportado_se_difiere_en_core_y_se_rechaza_si_llega_al_plugin() {
    let declared = support(&[TEXT_FILTER]);
    let projected = declared.project(&full(), SOURCE, None).unwrap();
    assert_eq!(projected.upstream.q, "llama");
    assert!(projected.upstream.capabilities.is_empty());
    assert!(projected.deferred_filters.contains("capabilities"));
    assert_eq!(projected.full.capabilities.len(), 2);
    declared.require(&projected.upstream).unwrap();
    let mut direct = projected.upstream;
    direct.capabilities = vec!["text.generate".into()];
    let error = declared.require(&direct).unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedCapability);
    assert!(!error.retryable);
    let textless = support(&[]);
    assert_eq!(
        textless.require(&direct).unwrap_err().code,
        ErrorCode::UnsupportedCapability
    );
    let projected = textless.project(&full(), SOURCE, None).unwrap();
    assert!(projected.upstream.q.is_empty());
    assert!(projected.deferred_filters.contains("q"));
}

#[test]
fn import_only_rechaza_busqueda_incluso_sin_filtros() {
    let import_only = SearchSupport {
        search_applicable: false,
        supported_filters: BTreeSet::new(),
        limitations: vec!["import_only".into()],
    };
    let request = SourceSearch {
        source_id: SOURCE.into(),
        q: String::new(),
        capabilities: vec![],
        cursor: None,
        limit: 50,
    };
    assert_eq!(
        import_only.require(&request).unwrap_err().code,
        ErrorCode::UnsupportedCapability
    );
    assert_eq!(
        import_only.project(&full(), SOURCE, None).unwrap_err().code,
        ErrorCode::UnsupportedCapability
    );
    let mut contradictory = import_only;
    contradictory.supported_filters.insert(TEXT_FILTER.into());
    assert_eq!(
        contradictory.validate().unwrap_err().code,
        ErrorCode::ValidationFailed
    );
}

#[test]
fn namespaces_desconocidos_se_conservan_sin_inventar_soporte_de_core() {
    let declared = support(&[TEXT_FILTER, "acme.task_tag"]);
    let roundtrip: SearchSupport =
        serde_json::from_value(serde_json::to_value(&declared).unwrap()).unwrap();
    assert_eq!(roundtrip, declared);
    let projected = declared.project(&full(), SOURCE, None).unwrap();
    assert!(projected.deferred_filters.contains("formats"));
    assert!(projected.deferred_filters.contains("capabilities"));
    assert!(projected
        .limitations
        .contains(&"upstream_is_partial".into()));
    for invalid in [
        "format",
        "core.formats",
        "source.search.architectures",
        "source.search.q.bad",
        "acme.Bad",
    ] {
        assert!(support(&[invalid]).validate().is_err(), "{invalid}");
    }
}

#[test]
fn digest_conserva_cada_filtro_no_cursor_ni_limit_ni_orden_de_listas() {
    let declared = support(&[TEXT_FILTER, CAPABILITY_FILTER]);
    let base = declared.project(&full(), SOURCE, None).unwrap();
    for (field, replacement) in [
        ("q", json!("otro")),
        ("capabilities", json!([])),
        ("source_ids", json!([])),
        ("compatibility", json!("UNKNOWN")),
        ("architectures", json!([])),
        ("formats", json!([])),
        ("quantizations", json!([])),
        ("runtime_ids", json!([])),
        ("licenses", json!([])),
        ("parameter_count", json!({"minimum":null,"maximum":null})),
        ("size_bytes", json!({"minimum":null,"maximum":null})),
        ("trust_score", json!({"minimum":null,"maximum":null})),
        ("certification_statuses", json!([])),
        ("only_executable", json!(false)),
        ("only_recommended", json!(false)),
        ("show_incompatible", json!(true)),
    ] {
        let mut changed = query();
        changed[field] = replacement;
        let search: Search = serde_json::from_value(changed).unwrap();
        assert_ne!(
            declared
                .project(&search, SOURCE, None)
                .unwrap()
                .filter_digest,
            base.filter_digest,
            "{field}"
        );
    }
    let mut next = full();
    next.cursor = Some("different-federated-cursor".into());
    next.limit = 1;
    next.formats.reverse();
    next.capabilities.reverse();
    assert_eq!(
        declared
            .project(&next, SOURCE, Some("opaque-upstream".into()))
            .unwrap()
            .filter_digest,
        base.filter_digest
    );
}

#[test]
fn proyeccion_no_mutua_filtros_contradictorios_y_valida_antes_de_admitir() {
    let declared = support(&[TEXT_FILTER]);
    let mut query = full();
    query.compatibility = larm_domain::search::CompatibilityFilter::Incompatible;
    assert!(declared
        .project(&query, SOURCE, None)
        .unwrap()
        .full
        .is_contradictory());
    query.limit = 0;
    assert!(declared.project(&query, SOURCE, None).is_err());
    assert!(declared
        .project(&full(), "not-a-source-uuid", None)
        .is_err());
    assert!(declared
        .project(&full(), SOURCE, Some(String::new()))
        .is_err());
}
