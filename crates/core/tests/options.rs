use wes_core::{Data, Primitive, Shape, contracts::ContractRegistry};

#[test]
fn options_are_explicit_nested_covariant_and_validate_contents() {
    let registry = ContractRegistry::new();
    let option = registry.resolve("Option<Int>").unwrap();
    assert_eq!(
        option.shape(),
        Shape::Option(Box::new(Shape::Primitive(Primitive::Int)))
    );
    assert!(option.issues(&Data::Option(None)).is_empty());
    assert!(
        option
            .issues(&Data::Option(Some(Box::new(Data::Int(0)))))
            .is_empty()
    );
    assert!(!option.issues(&Data::Int(0)).is_empty());
    assert!(
        !option
            .issues(&Data::Option(Some(Box::new(Data::Text("0".into())))))
            .is_empty()
    );
    let nested = registry.resolve("Option<Option<Int>>").unwrap();
    assert!(
        nested
            .issues(&Data::Option(Some(Box::new(Data::Option(None)))))
            .is_empty()
    );
    assert!(
        option
            .shape()
            .is_assignable_to(&Shape::Option(Box::new(Shape::Unknown)))
    );
}

#[test]
fn nullable_and_optional_record_fields_have_different_presence_rules() {
    let mut registry = ContractRegistry::new();
    registry.load("types:\n  Row:\n    base: Record\n    fields:\n      required: Option<Int>\n      absent: {type: Option<Int>, optional: true}\n").unwrap();
    let row = registry.resolve("Row").unwrap();
    assert_eq!(
        row.issues(&Data::Record(Default::default()))[0].path,
        "/required"
    );
    assert!(
        row.issues(&Data::Record(
            [("required".into(), Data::Option(None))].into()
        ))
        .is_empty()
    );
    let invalid = Data::Record(
        [(
            "required".into(),
            Data::Option(Some(Box::new(Data::Text("bad".into())))),
        )]
        .into(),
    );
    assert_eq!(row.issues(&invalid)[0].path, "/required");
    assert!(row.issues_with_cancel(&invalid, &|| true).is_err());
}
