use std::cell::Cell;
use wes_core::{
    Data,
    contracts::{ContractRegistry, TypeExpression},
};

fn text(value: &str) -> Data {
    Data::Text(value.into())
}
fn decimal(value: &str) -> Data {
    Data::Decimal(value.parse().unwrap())
}

#[test]
fn tutorial_package_resolves_forward_references() {
    let mut registry = ContractRegistry::new();
    let added = registry
        .load(include_str!("../../../tests/fixtures/catalog.types.yaml"))
        .unwrap();
    assert!(!added.is_empty());
    for name in added.keys() {
        assert_eq!(registry.resolve(name).unwrap().name(), name);
    }
}

#[test]
fn failed_package_is_atomic_even_after_a_valid_definition() {
    let mut registry = ContractRegistry::new();
    let before = registry.snapshot().keys().cloned().collect::<Vec<_>>();
    assert!(
        registry
            .load("types:\n  Fine: {base: Text}\n  Broken: {base: Missing}")
            .is_err()
    );
    assert_eq!(
        registry.snapshot().keys().cloned().collect::<Vec<_>>(),
        before
    );
    assert!(registry.resolve("Fine").is_err());
}

#[test]
fn nested_validation_reports_escaped_paths_and_optional_absence() {
    let mut registry = ContractRegistry::new();
    registry.load("types:\n  Item:\n    base: Record\n    fields:\n      a/b~c: Int\n      note: {type: Text, optional: true}\n").unwrap();
    let items = registry.resolve("List<Item>").unwrap();
    let value = Data::List(vec![Data::Record([("a/b~c".into(), text("bad"))].into())]);
    let issues = items.issues(&value);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].path, "/0/a~1b~0c");
    let value = Data::List(vec![Data::Record(
        [
            ("a/b~c".into(), Data::Int(4)),
            ("extra".into(), text("kept")),
        ]
        .into(),
    )]);
    assert!(items.issues(&value).is_empty());
    let empty = Data::List(vec![Data::Record(Default::default())]);
    assert_eq!(items.issues(&empty)[0].path, "/0/a~1b~0c");
}

#[test]
fn structural_subtyping_and_container_variance() {
    let mut registry = ContractRegistry::new();
    registry.load("types:\n  Category: {base: Text, enum: [books, games]}\n  Books: {base: Category, enum: [books]}\n  Independent: {base: Text, enum: [books]}\n").unwrap();
    let subtype = |a, b| {
        registry
            .resolve(a)
            .unwrap()
            .is_subtype_of(&registry.resolve(b).unwrap())
    };
    assert!(subtype("Books", "Category"));
    assert!(subtype("Independent", "Books"));
    assert!(subtype("List<Books>", "List<Category>"));
    assert!(subtype("Iter<Books>", "Iter<Category>"));
    assert!(subtype("Option<Books>", "Option<Category>"));
    assert!(subtype("Map<Text, Books>", "Map<Text, Category>"));
    assert!(!subtype("Map<Books, Int>", "Map<Category, Int>"));
    assert!(!subtype("Int", "Decimal"));
    assert!(subtype("Int", "Unknown"));
    assert!(!subtype("Unknown", "Int"));
}

#[test]
fn inherited_constraints_cannot_be_weakened() {
    for package in [
        "types:\n A: {base: Int, min: 2}\n B: {base: A, min: 1}",
        "types:\n A: {base: Int, max: 2}\n B: {base: A, max: 3}",
        "types:\n A: {base: Text, enum: [a]}\n B: {base: A, enum: [a, b]}",
        "types:\n A: {base: Record, fields: {id: Int}}\n B: {base: A, fields: {id: {type: Int, optional: true}}}",
        "types:\n A: {base: Record, fields: {id: Int}}\n B: {base: A, fields: {id: Text}}",
        "types:\n A: {base: Text, minLength: 3}\n B: {base: A, minLength: 2}",
        "types:\n A: {base: List<Int>, maxItems: 2}\n B: {base: A, maxItems: 3}",
    ] {
        let mut registry = ContractRegistry::new();
        assert!(registry.load(package).is_err(), "{package}");
        assert!(registry.resolve("A").is_err());
    }
}

#[test]
fn scalar_constraints_preserve_exact_numbers_and_do_not_coerce() {
    let mut registry = ContractRegistry::new();
    registry.load("types:\n Precise: {base: Decimal, min: 0.123456789012345678901234567890}\n Flag: {base: Bool, enum: [true]}\n Amount: {base: Decimal, enum: [12.50]}\n").unwrap();
    let precise = registry.resolve("Precise").unwrap();
    assert!(
        precise
            .issues(&decimal("0.123456789012345678901234567890"))
            .is_empty()
    );
    assert!(
        !precise
            .issues(&decimal("0.123456789012345678901234567889"))
            .is_empty()
    );
    assert!(!precise.issues(&Data::Int(1)).is_empty());
    assert!(
        registry
            .resolve("Amount")
            .unwrap()
            .issues(&decimal("12.5"))
            .is_empty()
    );
    assert!(
        !registry
            .resolve("Flag")
            .unwrap()
            .issues(&text("true"))
            .is_empty()
    );
}

#[test]
fn text_length_counts_code_points_and_patterns_search() {
    let mut registry = ContractRegistry::new();
    registry.load("types:\n One: {base: Text, minLength: 1, maxLength: 1}\n Book: {base: Text, pattern: book}\n").unwrap();
    assert!(
        registry
            .resolve("One")
            .unwrap()
            .issues(&text("😀"))
            .is_empty()
    );
    assert!(
        !registry
            .resolve("One")
            .unwrap()
            .issues(&text("e\u{301}"))
            .is_empty()
    );
    assert!(
        registry
            .resolve("Book")
            .unwrap()
            .issues(&text("notebook"))
            .is_empty()
    );
    assert!(
        registry
            .load("types: {Unsafe: {base: Text, pattern: '(?=x)'}}")
            .is_err()
    );
}

#[test]
fn invalid_schemas_are_rejected() {
    for package in [
        "types: {A: {base: A}}",
        "types: {A: {base: B}, B: {base: A}}",
        "types: {Text: {base: Int}}",
        "types: {List: {base: Int}}",
        "types: {A: {base: Text, min: 1}}",
        "types: {A: {base: Text, enum: []}}",
        "types: {A: {base: Int, min: 3, max: 2}}",
        "types: {A: {base: Int, enum: ['1']}}",
        "types: {A: {base: Bool, enum: [yes]}}",
        "version: '1'\ntypes: {}",
        "types: {A: {base: Int, min: 0x10}}",
        "types: {A: {base: Decimal, min: .inf}}",
        "types: {A: {base: Text, typo: 2}}",
        "types: {A: {base: Text, maxLength: 2147483648}}",
        "types: {A: {base: Record, fields: {id: {type: Int, optional: 'true'}}}}",
        "types: {A: {base: Text, minLength: 3, enum: [a]}}",
        "types: {A: {base: Int, fields: {id: Int}}}",
        "other: {}",
        "[]",
    ] {
        assert!(ContractRegistry::new().load(package).is_err(), "{package}");
    }
}

#[test]
fn type_expressions_validate_structure_and_constructor_arity() {
    let registry = ContractRegistry::new();
    assert_eq!(
        TypeExpression::parse("Map < Text, List < Int > >")
            .unwrap()
            .to_string(),
        "Map<Text, List<Int>>"
    );
    for expression in [
        "List<>",
        "List<Int,Text>",
        "Map<Int,Int>",
        "Map<Text>",
        "Int<Text>",
        "Int trailing",
        "List<Int",
        "",
    ] {
        assert!(registry.resolve(expression).is_err(), "{expression}");
    }
    assert!(
        registry
            .resolve(&format!("{}Int{}", "List<".repeat(70), ">".repeat(70)))
            .is_err()
    );
    assert!(TypeExpression::parse(&"a".repeat(4097)).is_err());
}

#[test]
fn option_and_iter_do_not_silently_accept_old_data_representations() {
    let registry = ContractRegistry::new();
    for (expression, code) in [("Option<Int>", "TYP005"), ("Iter<Int>", "TYP008")] {
        assert_eq!(
            registry.resolve(expression).unwrap().issues(&Data::Int(1))[0].code,
            code
        );
    }
}

#[test]
fn validation_is_bounded_and_cancellation_is_a_separate_outcome() {
    let list = ContractRegistry::new().resolve("List<Int>").unwrap();
    let large = Data::List(vec![Data::Int(1); 100_001]);
    assert_eq!(list.issues(&large).last().unwrap().code, "TYP006");
    let checks = Cell::new(0);
    let cancelled = || {
        checks.set(checks.get() + 1);
        checks.get() > 10
    };
    assert!(list.issues_with_cancel(&large, &cancelled).is_err());
    assert_eq!(checks.get(), 11);
    assert_eq!(list.issues(&Data::List(vec![text("bad"); 200])).len(), 100);
}

#[test]
fn built_in_error_and_cancel_contracts_have_portable_required_fields() {
    let registry = ContractRegistry::new();
    assert_eq!(
        registry.resolve("Error").unwrap().shape(),
        registry.resolve("Cancellation").unwrap().shape()
    );
    assert_eq!(
        registry
            .resolve("Error")
            .unwrap()
            .issues(&Data::Record(Default::default()))
            .len(),
        6
    );
}

#[test]
fn self_contained_imports_reuse_equal_types_preserve_capture_and_reject_conflicts_atomically() {
    let mut registry = wes_core::contracts::ContractRegistry::new();
    registry
        .load("types: {Row: {base: Record, fields: {name: Text}}}")
        .unwrap();
    registry
        .import_package(
            "types: {Row: {base: Record, fields: {name: Text}}, Selection: {base: Text}}",
        )
        .unwrap();
    let capture = registry.capture("Selection").unwrap();
    let mut restored = wes_core::contracts::ContractRegistry::new();
    for source in capture.packages {
        restored.load(&source).unwrap();
    }
    assert!(restored.resolve("Selection").is_ok());
    assert!(
        registry
            .import_package("types: {Row: {base: Int}, Other: {base: Text}}")
            .is_err()
    );
    assert!(registry.resolve("Other").is_err());
}

#[test]
fn constraint_hints_and_validation_show_resolved_rules_not_generated_aliases() {
    let mut registry = ContractRegistry::new();
    registry.load("types:\n  ApiType3: {base: Int, min: 1, max: 50}\n  Status: {base: Text, enum: [queued, done]}\n  Short: {base: Text, minLength: 2, maxLength: 4}\n  Few: {base: List<Int>, minItems: 1, maxItems: 3}\n").unwrap();
    let limit = registry.resolve("ApiType3").unwrap();
    assert_eq!(limit.constraint_hints(), ["number: 1..50 (inclusive)"]);
    for n in [1, 50] {
        assert!(limit.issues(&Data::Int(n)).is_empty());
    }
    let issue = &limit.issues(&Data::Int(99))[0];
    assert!(issue.message.contains("1..50 (inclusive)"));
    assert!(!issue.message.contains("ApiType3"));
    assert!(!issue.message.contains("99"));
    let optional = registry.resolve("Option<ApiType3>").unwrap();
    assert_eq!(optional.constraint_hints(), limit.constraint_hints());
    let status = registry.resolve("Status").unwrap();
    assert!(status.constraint_hints()[0].contains("\"queued\", \"done\""));
    assert!(
        status.issues(&text("private-rejected-value"))[0]
            .message
            .contains("allowed values")
    );
    assert!(
        !status.issues(&text("private-rejected-value"))[0]
            .message
            .contains("private-rejected-value")
    );
    assert!(registry.resolve("Short").unwrap().constraint_hints()[0].contains("2..4"));
    assert!(registry.resolve("Few").unwrap().constraint_hints()[0].contains("1..3"));
}
