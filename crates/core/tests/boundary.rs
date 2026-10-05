use wes_core::{
    Data, Primitive, Provenance, Shape, Value,
    contracts::{
        ContractRegistry,
        boundary::{self, BoundaryError},
    },
};

fn registry() -> ContractRegistry {
    let mut r = ContractRegistry::new();
    r.load("types: {Positive: {base: Int, min: 1}, Item: {base: Record, fields: {id: Int}}}")
        .unwrap();
    r
}

#[test]
fn only_literal_text_is_contextually_read_as_a_number() {
    let r = registry();
    let guards = vec![r.resolve("Positive").unwrap()];
    let text = Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text("2".into()),
        Provenance::default(),
    )
    .unwrap();
    assert_eq!(
        boundary::literal("value", &guards, &Shape::Unknown, &text)
            .unwrap()
            .data(),
        &Data::Int(2)
    );
    let BoundaryError::Invalid { issues, .. } =
        boundary::require("a/b~c", &guards, &Shape::Unknown, &text, &|| false).unwrap_err()
    else {
        panic!("validation failure")
    };
    assert_eq!(issues[0].path, "/arguments/a~1b~0c");
}

#[test]
fn unknown_outer_contract_does_not_erase_inner_shape() {
    let r = registry();
    let guards = vec![
        r.resolve("Unknown").unwrap(),
        r.resolve("Positive").unwrap(),
    ];
    assert_eq!(
        boundary::shape(&guards, &Shape::Unknown).unwrap(),
        Shape::Primitive(Primitive::Int)
    );
    assert_eq!(
        boundary::shape(&guards, &Shape::Primitive(Primitive::Decimal))
            .unwrap_err()
            .code,
        "TYP009"
    );
}

#[test]
fn structured_result_refinement_preserves_producer_data_shape_and_provenance() {
    let r = registry();
    let guards = vec![r.resolve("Item").unwrap()];
    let producer = Value::new(
        Shape::Unknown,
        Data::Record(
            [
                ("id".into(), Data::Int(7)),
                ("extra".into(), Data::Bool(true)),
            ]
            .into(),
        ),
        Provenance::default().with_fact("source", "fixture"),
    )
    .unwrap();
    let argument =
        boundary::require("value", &guards, &Shape::Unknown, &producer, &|| false).unwrap();
    assert_eq!(producer.shape(), &Shape::Unknown);
    assert_eq!(argument.shape(), &guards[0].shape());
    assert!(std::ptr::eq(argument.data(), producer.data()));
    assert_eq!(argument.provenance(), producer.provenance());
    assert!(matches!(
        boundary::require("value", &guards, &Shape::Unknown, &producer, &|| true),
        Err(BoundaryError::Cancelled(_))
    ));
}
