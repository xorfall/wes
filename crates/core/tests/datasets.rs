use wes_core::{
    Data, DatasetRef, Primitive, Shape,
    contracts::{ContractRegistry, ResolvedContractBundle, SnapshotLimits},
};
fn reference(digest: &str, records: u64) -> DatasetRef {
    DatasetRef::new(
        "bd111111-1111-4111-8111-111111111111".into(),
        "bd222222-2222-4222-8222-222222222222".into(),
        1,
        "bd333333-3333-4333-8333-333333333333".into(),
        format!("sha256:{}", "1".repeat(64)),
        128,
        digest.into(),
        records,
        1,
    )
    .unwrap()
}
#[test]
fn descriptors_preserve_exact_u64_and_refuse_paths_noncanonical_numbers_or_unknown_fields() {
    let reference = reference(&format!("sha256:{}", "2".repeat(64)), u64::MAX);
    let wire = serde_json::to_value(&reference).unwrap();
    assert_eq!(wire["records"], "18446744073709551615");
    assert_eq!(
        serde_json::from_value::<DatasetRef>(wire.clone()).unwrap(),
        reference
    );
    for (key, invalid) in [
        ("store", serde_json::json!("../foreign")),
        ("generation", serde_json::json!(1)),
        ("generation", serde_json::json!("01")),
        ("manifestDigest", serde_json::json!("sha256:xx")),
        ("records", serde_json::json!("18446744073709551616")),
        ("authorizationGeneration", serde_json::json!("0")),
        ("extra", serde_json::json!(true)),
    ] {
        let mut value = wire.clone();
        value[key] = invalid;
        assert!(
            serde_json::from_value::<DatasetRef>(value).is_err(),
            "accepted invalid {key}"
        );
    }
}
#[test]
fn dataset_is_not_an_implicit_list_and_requires_exact_full_element_contract() {
    let mut registry = ContractRegistry::new();
    registry
        .load("types: {Small: {base: Int, min: 0, max: 9}}")
        .unwrap();
    let int = registry.resolve("Int").unwrap();
    let dataset = registry.resolve("Dataset<Int>").unwrap();
    let small = registry.resolve("Dataset<Small>").unwrap();
    let data = Data::Dataset(reference(int.digest(), 10).into());
    assert!(dataset.issues(&data).is_empty());
    assert!(!small.issues(&data).is_empty());
    assert!(
        !registry
            .resolve("List<Int>")
            .unwrap()
            .issues(&data)
            .is_empty()
    );
    assert!(
        !dataset
            .shape()
            .is_assignable_to(&Shape::List(Box::new(Shape::Primitive(Primitive::Int))))
    );
    let snapshot =
        ResolvedContractBundle::capture(dataset.clone(), SnapshotLimits::default()).unwrap();
    let restored =
        ResolvedContractBundle::decode(snapshot.encoded(), SnapshotLimits::default()).unwrap();
    assert_eq!(restored.root().digest(), dataset.digest());
    assert_eq!(restored.root().shape(), dataset.shape());
    assert!(restored.root().issues(&data).is_empty());
}
#[test]
fn isolated_schema_lookup_interns_shared_children_across_independent_bundles() {
    let mut registry = ContractRegistry::new();
    registry.load("types: {Digest: {base: Text, pattern: '^[a-f0-9]{64}$'}, Left: {base: Record, fields: {digest: Digest}}, Right: {base: Record, fields: {digest: Digest}}}").unwrap();
    let bundles = ["Left", "Right"].map(|name| {
        ResolvedContractBundle::capture(registry.resolve(name).unwrap(), Default::default())
            .unwrap()
    });
    let isolated = ContractRegistry::from_resolved(&bundles).unwrap();
    let left = isolated.resolve("Left").unwrap();
    let right = isolated.resolve("Right").unwrap();
    let wes_core::contracts::ContractKind::Record(left) = left.kind() else {
        panic!()
    };
    let wes_core::contracts::ContractKind::Record(right) = right.kind() else {
        panic!()
    };
    assert!(std::sync::Arc::ptr_eq(
        &left["digest"].contract,
        &right["digest"].contract
    ));
    assert!(std::sync::Arc::ptr_eq(
        &left["digest"].contract,
        &isolated.resolve("Digest").unwrap()
    ));
}

#[test]
fn captured_dataset_metadata_validates_and_projects_only_supported_public_descriptors() {
    let mut registry = ContractRegistry::new();
    registry
        .load("types: {Result: {base: Record, fields: {outputs: 'Dataset<Int>', label: Text}}}")
        .unwrap();
    let contract = registry.resolve("Result").unwrap();
    let metadata = wes_core::contracts::metadata::ValueMetadata::capture(&contract);
    metadata.validate().unwrap();
    metadata.project("/f:outputs").unwrap().validate().unwrap();
    let wire = metadata.wire().unwrap();
    wire.validate_wire().unwrap();
    let json = serde_json::to_value(wire).unwrap();
    assert!(
        json["fields"].get("/f:outputs").is_none(),
        "a Dataset container does not invent scalar presentation metadata"
    );
    assert!(json["fields"].get("/f:label").is_some());
}
#[test]
fn inline_and_storable_boundaries_cover_nested_refs_absent_options_and_executable_shapes() {
    let r = reference(&format!("sha256:{}", "2".repeat(64)), 0);
    let data = Data::List(vec![Data::Option(Some(Box::new(Data::Dataset(r.into()))))]);
    assert!(!data.is_inline());
    assert!(data.is_storable_snapshot());
    let shape = Shape::Option(Box::new(Shape::Dataset(Box::new(Shape::Primitive(
        Primitive::Int,
    )))));
    assert!(!shape.is_inline());
    assert!(shape.is_storable_snapshot());
    assert!(Data::Option(None).is_inline());
    assert!(
        !Shape::Dataset(Box::new(Shape::Iter(Box::new(Shape::Unknown)))).is_storable_snapshot()
    );
    assert!(
        !Shape::Dataset(Box::new(Shape::Dataset(Box::new(Shape::Unknown)))).is_storable_snapshot()
    );
    let nested = (0..130).fold(Shape::Unknown, |shape, _| Shape::Option(Box::new(shape)));
    assert!(!nested.is_inline());
    assert!(!nested.is_storable_snapshot());
}
