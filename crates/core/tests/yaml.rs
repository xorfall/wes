use wes_core::contracts::preflight_package;

#[test]
fn ambiguous_unsafe_or_multi_document_yaml_is_rejected() {
    for text in [
        "a: 1\na: 2",
        "a: &a [*a]",
        "types: !!java/object {}",
        "a: null",
        "a: Null",
        "a: NULL",
        "a: ~",
        "a: !!bool garbage",
        "a: 1\n---\na: 2",
        "",
        "a: !!set {}",
        "[unclosed",
    ] {
        assert!(preflight_package(text).is_err(), "{text:?}");
    }
}

#[test]
fn quotes_and_explicit_standard_tags_preserve_text_intent() {
    for text in [
        "a: !!str true",
        "a: 'null'",
        "a: !!bool TRUE",
        "a: !!map {}",
        "a: !!seq []",
    ] {
        assert!(preflight_package(text).is_ok(), "{text:?}");
    }
}

#[test]
fn byte_depth_and_event_budgets_fail_explicitly() {
    assert!(preflight_package(&"a".repeat(1_048_577)).is_err());
    let nested = format!("{}0{}", "[".repeat(65), "]".repeat(65));
    assert!(preflight_package(&nested).is_err());
    let many = format!("[{}0]", "0,".repeat(20_001));
    assert!(preflight_package(&many).is_err());
}

#[test]
fn complete_tutorial_is_parseable_without_external_io() {
    preflight_package(include_str!("../../../tests/fixtures/catalog.types.yaml")).unwrap();
}
