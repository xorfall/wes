use std::str::FromStr;

use indexmap::IndexMap;
use wes_core::{Data, Decimal, Primitive, Provenance, RecordShape, Shape, TimeParts, Value};

fn primitive(p: Primitive) -> Shape {
    Shape::Primitive(p)
}

fn record(name: &str, fields: &[(&str, Shape)]) -> Shape {
    Shape::Record(
        RecordShape::new(name, fields.iter().map(|(k, v)| (k.to_string(), v.clone()))).unwrap(),
    )
}

#[test]
fn structural_records_ignore_names_and_order_but_keep_display_order() {
    let a = record(
        "Order",
        &[
            ("id", primitive(Primitive::Int)),
            ("title", primitive(Primitive::Text)),
        ],
    );
    let b = record(
        "Other",
        &[
            ("title", primitive(Primitive::Text)),
            ("id", primitive(Primitive::Int)),
        ],
    );
    assert_eq!(a, b);
    assert!(a.is_assignable_to(&b) && b.is_assignable_to(&a));
    let Shape::Record(fields) = a else {
        panic!("record")
    };
    assert_eq!(
        fields.fields().map(|(key, _)| key).collect::<Vec<_>>(),
        ["id", "title"]
    );
}

#[test]
fn duplicate_shape_fields_are_rejected() {
    assert!(
        RecordShape::new(
            "A",
            [("id".into(), Shape::Unknown), ("id".into(), Shape::Unknown)]
        )
        .is_err()
    );
}

#[test]
fn width_depth_and_covariant_lists() {
    let small = record("Small", &[("id", primitive(Primitive::Int))]);
    let wide = record(
        "Wide",
        &[("id", primitive(Primitive::Int)), ("extra", Shape::Unknown)],
    );
    assert!(wide.is_assignable_to(&small));
    assert!(!small.is_assignable_to(&wide));
    assert!(Shape::List(Box::new(wide)).is_assignable_to(&Shape::List(Box::new(small))));
}

#[test]
fn unknown_is_top_not_an_implicit_cast() {
    let text = primitive(Primitive::Text);
    assert!(text.is_assignable_to(&Shape::Unknown));
    assert!(Shape::Unknown.is_assignable_to(&Shape::Unknown));
    assert!(!Shape::Unknown.is_assignable_to(&text));
    assert!(!primitive(Primitive::Int).is_assignable_to(&primitive(Primitive::Decimal)));
}

#[test]
fn exact_integer_conversion_does_not_expand_extreme_decimal_exponents() {
    for (text, expected) in [
        ("0e2147483647", Some(0)),
        ("1e2147483647", None),
        ("1e-2147483647", None),
        ("1.00", Some(1)),
        ("-9223372036854775808.0", Some(i64::MIN)),
        ("1E+3", Some(1000)),
        ("1.1", None),
    ] {
        assert_eq!(
            text.parse::<Decimal>().unwrap().exact_i64(),
            expected,
            "{text}"
        );
    }
}

#[test]
fn bounded_plain_decimal_text_preserves_scale_and_rejects_exponent_sized_allocation() {
    for (source, expected) in [
        ("1E+3", "1000"),
        ("0E+3", "0"),
        ("0.000", "0.000"),
        ("-1E-7", "-0.0000001"),
        ("12.3400", "12.3400"),
        ("-0E+1000", "0"),
    ] {
        let number: Decimal = source.parse().unwrap();
        assert_eq!(number.plain_text(expected.len()), Some(expected.into()));
        assert_eq!(number.plain_text(expected.len() - 1), None);
    }
    for source in ["1e2147483647", "1e-2147483647", "0e-2147483647"] {
        assert_eq!(source.parse::<Decimal>().unwrap().plain_text(1024), None);
    }
    assert_eq!(
        "0e2147483647".parse::<Decimal>().unwrap().plain_text(1),
        Some("0".into())
    );
}

#[test]
fn shallow_construction_does_not_claim_item_validation() {
    let data = Data::List(vec![Data::Text("not an integer".into())]);
    assert!(
        Value::new(
            Shape::List(Box::new(primitive(Primitive::Int))),
            data,
            Provenance::default()
        )
        .is_ok()
    );
    assert!(
        Value::new(
            primitive(Primitive::Int),
            Data::Text("2".into()),
            Provenance::default()
        )
        .is_err()
    );
}

#[test]
fn referenced_refinement_preserves_data_and_provenance() {
    let source = Value::new(
        Shape::Unknown,
        Data::Int(2),
        Provenance::default().with_fact("source", "fixture"),
    )
    .unwrap();
    let refined = source.with_shape(primitive(Primitive::Int)).unwrap();
    assert_eq!(refined.data(), source.data());
    assert_eq!(refined.provenance(), source.provenance());
    assert_eq!(source.shape(), &Shape::Unknown);
}

#[test]
fn decimal_precision_and_scale_are_separate_from_numeric_comparison() {
    let a = Decimal::from_str("123456789012345678901234567890.1200").unwrap();
    let b = Decimal::from_str("123456789012345678901234567890.12").unwrap();
    assert_eq!(a.scale(), 4);
    assert_ne!(a, b);
    assert_eq!(a.numeric_cmp(&b), std::cmp::Ordering::Equal);
    assert_eq!(a.to_string(), "123456789012345678901234567890.1200");
    assert!(Decimal::from_str("NaN").is_err());
    assert!(Decimal::from_str("1e2147483649").is_err());
}

#[test]
fn negative_duration_representation_is_not_unsigned() {
    let half_second_before = TimeParts::new(-1, 500_000_000).unwrap();
    assert_eq!(half_second_before.seconds(), -1);
    assert_eq!(half_second_before.nanos(), 500_000_000);
    assert!(TimeParts::new(0, 1_000_000_000).is_err());
}

#[test]
fn ordered_data_equality_is_structural() {
    let a = Data::Record(IndexMap::from([
        ("b".into(), Data::Int(2)),
        ("a".into(), Data::Int(1)),
    ]));
    let b = Data::Record(IndexMap::from([
        ("a".into(), Data::Int(1)),
        ("b".into(), Data::Int(2)),
    ]));
    assert_eq!(a, b);
}

#[test]
fn provenance_facts_intersect_but_cautions_union() {
    let a = Provenance::default()
        .with_fact("source", "a")
        .with_fact("common", "yes")
        .cautioned(["first".into()]);
    let b = Provenance::default()
        .with_fact("source", "b")
        .with_fact("common", "yes")
        .cautioned(["second".into()]);
    let merged = a.merge(&b);
    assert_eq!(merged.fact("source"), None);
    assert_eq!(merged.fact("common"), Some("yes"));
    assert_eq!(merged.cautions().len(), 2);
    assert_eq!(a.merge(&b), b.merge(&a));
    assert_eq!(a.merge(&a), a);
}

#[test]
fn producer_facts_override_input_consensus_without_erasing_cautions() {
    let carried = Provenance::default()
        .with_fact("source", "input")
        .with_fact("unit", "items")
        .cautioned(["unchecked".into()]);
    let own = Provenance::default().with_fact("source", "output");
    let result = own.inheriting(&carried);
    assert_eq!(result.fact("source"), Some("output"));
    assert_eq!(result.fact("unit"), Some("items"));
    assert!(result.cautions().contains("unchecked"));
    assert!(Provenance::agreed_by([]).is_empty());
    assert_eq!(Provenance::agreed_by([&carried]), carried);
}
