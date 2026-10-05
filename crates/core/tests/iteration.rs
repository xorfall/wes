use std::sync::Arc;
use wes_core::{
    Data, IterMode, IterStage, IterValue, Provenance, Shape, Value, contracts::ContractRegistry,
};
#[test]
fn atomic_recipes_capture_item_contracts_without_scanning_source() {
    let mut registry = ContractRegistry::new();
    registry.load("types:\n  Line: {base: Text, maxLength: 3}\niterators:\n  shortLines: {input: Text, output: 'Iter<Line>', mode: lines}\n").unwrap();
    assert_eq!(
        registry.iterators()["shortLines"]
            .output
            .shape()
            .to_string(),
        "Iter<Text>"
    );
    let capture = registry.capture("Line").unwrap();
    let value = IterValue::new(
        Value::new(
            Shape::Unknown,
            Data::Text("too long\n".into()),
            Provenance::default(),
        )
        .unwrap(),
        IterMode::Lines,
        None,
        vec![IterStage::Check(capture)],
    )
    .unwrap();
    let value = Value::new(
        Shape::Iter(Box::new(value.item_shape().clone())),
        Data::Iter(Arc::new(value)),
        Provenance::default(),
    )
    .unwrap();
    assert!(
        registry
            .resolve("Iter<Line>")
            .unwrap()
            .issues(value.data())
            .is_empty()
    );
    let count = registry.sources().len();
    assert!(registry.load("types:\n  NewType: {base: Int}\niterators:\n  broken: {input: Int, output: 'Iter<Int>', mode: lines}\n").is_err());
    assert!(registry.resolve("NewType").is_err());
    assert_eq!(registry.sources().len(), count);
}
#[test]
fn iter_is_distinct_from_lists_and_invalid_extraction_is_refused() {
    let registry = ContractRegistry::new();
    assert!(
        !registry
            .resolve("Iter<Int>")
            .unwrap()
            .issues(&Data::List(vec![Data::Int(1)]))
            .is_empty()
    );
    assert!(
        !Shape::List(Box::new(Shape::Unknown))
            .is_assignable_to(&Shape::Iter(Box::new(Shape::Unknown)))
    );
    let source = Value::new(
        Shape::Unknown,
        Data::Text("abc".into()),
        Provenance::default(),
    )
    .unwrap();
    assert!(IterValue::new(source.clone(), IterMode::Items, None, vec![]).is_err());
    assert!(IterValue::new(source.clone(), IterMode::Split, Some("".into()), vec![]).is_err());
    assert!(IterValue::new(source, IterMode::Matches, Some("[".into()), vec![]).is_err());
}

#[test]
fn oversized_compiled_recipe_regex_is_rejected_atomically_at_load() {
    // Short Unicode patterns can compile to much more than their source byte size.
    let pattern = r"\w{100}";
    assert!(regex::Regex::new(pattern).is_ok());
    assert!(
        regex::RegexBuilder::new(pattern)
            .size_limit(1024 * 1024)
            .build()
            .is_err()
    );
    let mut registry = ContractRegistry::new();
    let package = format!(
        "types:\n  NewLine: {{base: Text}}\niterators:\n  large: {{input: Text, output: 'Iter<NewLine>', mode: matches, pattern: '{pattern}'}}\n"
    );
    assert!(registry.load(&package).is_err());
    assert!(registry.resolve("NewLine").is_err());
    assert!(registry.iterators().is_empty());
    assert!(registry.sources().is_empty());
}

#[test]
fn compiled_patterns_are_bounded_run_local_and_not_value_identity() {
    use wes_core::IterRegexCache;
    let mut cache = IterRegexCache::default();
    let make = |pattern: &str, cache: &mut IterRegexCache| {
        IterValue::new_cached(
            Value::new(
                Shape::Unknown,
                Data::Text("a b a".into()),
                Provenance::default(),
            )
            .unwrap(),
            IterMode::Matches,
            Some(pattern.into()),
            vec![],
            cache,
        )
    };
    let first = make("a", &mut cache).unwrap();
    let again = make("a", &mut cache).unwrap();
    assert!(Arc::ptr_eq(
        &first.compiled_regex().unwrap().unwrap(),
        &again.compiled_regex().unwrap().unwrap()
    ));
    let separate = make("a", &mut IterRegexCache::default()).unwrap();
    assert!(!Arc::ptr_eq(
        &first.compiled_regex().unwrap().unwrap(),
        &separate.compiled_regex().unwrap().unwrap()
    ));
    assert_eq!(first, separate);
    assert_eq!(format!("{first:?}"), format!("{separate:?}"));
    assert!(!format!("{first:?}").contains("regex:"));
    for pattern in ["b", "c", "d", "e"] {
        make(pattern, &mut cache).unwrap();
    }
    let evicted = make("a", &mut cache).unwrap();
    assert!(!Arc::ptr_eq(
        &first.compiled_regex().unwrap().unwrap(),
        &evicted.compiled_regex().unwrap().unwrap()
    ));
    let compiled = again.compiled_regex().unwrap().unwrap();
    let weak = Arc::downgrade(&compiled);
    drop(compiled);
    drop(cache);
    assert!(
        weak.upgrade().is_none(),
        "plans must not retain compiled regex after run/cache ends"
    );
    let mut cache = IterRegexCache::default();
    assert!(make("[", &mut cache).is_err());
    assert!(make(&"a".repeat(16385), &mut cache).is_err());
    assert!(make("[a-z]{1000000}", &mut cache).is_err());
}
#[test]
fn iterator_failures_explain_pattern_and_source_without_dumping_the_pattern() {
    let source = || {
        Value::new(
            Shape::Unknown,
            Data::Text("sample".into()),
            Provenance::default(),
        )
        .unwrap()
    };
    let error = IterValue::new(source(), IterMode::Split, Some(String::new()), vec![])
        .unwrap_err()
        .to_string();
    assert!(error.contains("iter.chars") && error.contains("empty"));
    let error = IterValue::new(source(), IterMode::Matches, Some("(?=x)".into()), vec![])
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("look-around") && error.len() < 600,
        "{error}"
    );
    let error = IterValue::new(source(), IterMode::Split, Some("x".repeat(16385)), vec![])
        .unwrap_err()
        .to_string();
    assert!(error.contains("16384-byte"));
    let value = Value::new(Shape::Unknown, Data::Int(503), Provenance::default()).unwrap();
    let error = IterValue::new(value, IterMode::Words, None, vec![])
        .unwrap_err()
        .to_string();
    assert_eq!(error, "iter.words expects Text; received Int");
}

#[test]
fn capture_recipes_require_patterns_and_share_the_runtime_extraction_contract() {
    let types =
        "types: {Capture: {base: Record, fields: {match: Text, groups: 'List<Option<Text>>'}}}";
    let mut registry = ContractRegistry::new();
    assert!(registry.load(&format!("{types}\niterators: {{extract: {{input: Text, output: 'Iter<Capture>', mode: captures}}}}" )).is_err());
    assert!(registry.iterators().is_empty());
    registry.load(&format!("{types}\niterators: {{extract: {{input: Text, output: 'Iter<Capture>', mode: captures, pattern: '(a)?b'}}}}" )).unwrap();
    let recipe = &registry.iterators()["extract"];
    assert_eq!(recipe.mode.argument_kind(), Some("pattern"));
    let source = Value::new(
        Shape::Unknown,
        Data::Text("b".into()),
        Provenance::default(),
    )
    .unwrap();
    let value = IterValue::new(source, recipe.mode, recipe.argument.clone(), vec![]).unwrap();
    assert_eq!(value.source_item_shape(), &IterMode::capture_shape());
}
