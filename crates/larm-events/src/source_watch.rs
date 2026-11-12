//! Facade de clasificación estable de descubrimientos `source.sync` (P050).

pub use larm_domain::watchers::{
    classify_source_revision as classify, source_watch_dedupe_key as dedupe_key,
    SourceRevisionEvent,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_new_update_and_suppresses_the_same_revision() {
        assert_eq!(
            classify(None, "a", false),
            Some(SourceRevisionEvent::NewModel)
        );
        assert_eq!(
            classify(None, "a", true),
            Some(SourceRevisionEvent::NewCompatibleModel)
        );
        assert_eq!(
            classify(Some("a"), "b", false),
            Some(SourceRevisionEvent::ModelUpdate)
        );
        assert_eq!(classify(Some("a"), "a", false), None);
    }

    #[test]
    fn dedupe_key_is_deterministic_and_does_not_expose_upstream_identity() {
        let key = dedupe_key(
            "source",
            "private-model-name",
            "f".repeat(64).as_str(),
            SourceRevisionEvent::NewModel,
        );
        assert_eq!(key.len(), 64);
        assert!(!key.contains("private-model-name"));
        assert_eq!(
            key,
            dedupe_key(
                "source",
                "private-model-name",
                "f".repeat(64).as_str(),
                SourceRevisionEvent::NewModel
            )
        );
    }
}
