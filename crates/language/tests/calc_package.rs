use wes_language::calc::{Binary, DEFAULT_PACKAGE, Package};

#[test]
fn package_captures_grammar_and_signatures_without_retargeting_existing_users() {
    let original = Package::standard();
    let changed = DEFAULT_PACKAGE
        .replace(
            "operation: add, precedence: 5",
            "operation: add, precedence: 7",
        )
        .replace(
            "operation: range, min: 1, max: 3",
            "operation: range, min: 2, max: 2",
        );
    let next = Package::load(&changed).unwrap();
    assert_eq!(original.binary("+").unwrap().precedence, 5);
    assert_eq!(next.binary("+").unwrap().precedence, 7);
    assert_eq!(next.binary("+").unwrap().operation, Binary::Add);
    assert_eq!(next.operation("range").unwrap().min, 2);
    assert_eq!(original.operation("range").unwrap().min, 1);
    assert_eq!(next.source(), changed);
}

#[test]
fn malformed_versions_mappings_aliases_and_host_operations_are_rejected_atomically() {
    for source in [
        DEFAULT_PACKAGE.replace("version: 1", "version: 4"),
        DEFAULT_PACKAGE.replace("version: 1", "version: 1\nversion: 1"),
        DEFAULT_PACKAGE.replace("operation: call", "operation: host.eval"),
        DEFAULT_PACKAGE.replace("min: 3, max: 3", "min: 0, max: 100"),
        DEFAULT_PACKAGE.replace("  const: binding", "  const: function"),
        DEFAULT_PACKAGE.replace("  const: binding", "  const: recursive-production"),
        format!("{DEFAULT_PACKAGE}\nextra: &self [*self]"),
        "x".repeat(64 * 1024 + 1),
    ] {
        assert!(Package::load(&source).is_err());
    }
    assert_eq!(Package::standard().version(), 1);
}

#[test]
fn only_current_semantics_are_accepted() {
    for version in [0, 2, 3, 4, 99] {
        let source = DEFAULT_PACKAGE.replace("version: 1", &format!("version: {version}"));
        assert!(Package::load(&source).is_err(), "version {version}");
    }
    let current = Package::standard();
    assert!(current.iter_namespace());
    assert!(current.operation("httpStatus").is_some());
    assert!(current.operation("iter.items").is_some());
}

#[test]
fn every_standard_operation_has_help_matching_its_registered_arity() {
    for (name, spec) in Package::standard().operations() {
        let help = spec.operation.help();
        assert_eq!(help.parameters.len(), usize::from(spec.max), "{name}");
        assert!(spec.min <= spec.max);
        assert!(
            !help.summary.is_empty()
                && !help.returns.is_empty()
                && !help.notes.is_empty()
                && !help.example.is_empty(),
            "{name}"
        );
    }
}
