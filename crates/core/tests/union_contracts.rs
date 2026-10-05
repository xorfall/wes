use wes_core::{Data, Shape, contracts::ContractRegistry};

fn registry() -> ContractRegistry {
    let mut registry = ContractRegistry::new();
    registry
        .load(include_str!("../../../examples/union-contracts/types.yaml"))
        .unwrap();
    registry
}
fn text(value: &str) -> Data {
    Data::Text(value.into())
}
fn count(value: i64) -> Data {
    Data::Record(
        [
            ("kind".into(), text("count")),
            ("count".into(), Data::Int(value)),
        ]
        .into(),
    )
}
#[test]
fn example_accepts_alternatives_without_weakening_members() {
    let registry = registry();
    let content = registry.resolve("Content").unwrap();
    assert_eq!(content.shape(), Shape::Unknown);
    for value in [
        text("hello"),
        Data::List(vec![count(2)]),
        Data::List(vec![]),
    ] {
        let before = value.clone();
        assert!(content.issues(&value).is_empty());
        assert_eq!(value, before);
    }
    for value in [
        Data::Bool(true),
        Data::List(vec![count(0)]),
        Data::List(vec![Data::Record(Default::default())]),
    ] {
        assert!(!content.issues(&value).is_empty());
    }
    assert_eq!(
        registry.resolve("Union<Int, Positive>").unwrap().shape(),
        registry.resolve("Int").unwrap().shape()
    );
}
#[test]
fn inclusion_is_structural_and_checks_every_actual_alternative() {
    let registry = registry();
    let subtype = |a, b| {
        registry
            .resolve(a)
            .unwrap()
            .is_subtype_of(&registry.resolve(b).unwrap())
    };
    assert!(subtype("Positive", "Union<Text, Int>"));
    assert!(subtype("Union<Int, Text>", "Union<Text, Int>"));
    assert!(subtype("Union<Positive, Text>", "Union<Text, Int>"));
    assert!(subtype("Content", "Unknown"));
    assert!(!subtype("Union<Positive, Text>", "Int"));
    assert!(!subtype("Union<Int, Text>", "Union<Text, Positive>"));
    assert!(!subtype("Unknown", "Content"));
    assert!(!subtype("Int", "Union<Text, Decimal>"));
}
#[test]
fn alternative_failure_cannot_reset_budget_or_ignore_cancellation() {
    let registry = registry();
    let contract = registry.resolve("Union<List<Int>, List<Text>>").unwrap();
    let value = Data::List(vec![text("first"), text("second")]);
    let mut budget = 4;
    let issues = contract
        .issues_with_budget(&value, &|| false, &mut budget)
        .unwrap();
    assert!(issues.iter().any(|issue| issue.code == "TYP006"));
    assert_eq!(budget, 0);
    assert!(contract.issues_with_cancel(&value, &|| true).is_err());
    assert!(contract.issues(&value).is_empty());
}

#[test]
fn shared_alternative_dag_has_bounded_shape_and_inclusion_work() {
    let mut registry = ContractRegistry::new();
    let mut package = String::from("types:\n  U0: {base: Int}\n");
    for n in 1..30 {
        package.push_str(&format!(
            "  U{n}: {{base: 'Union<U{}, U{}>'}}\n",
            n - 1,
            n - 1
        ));
    }
    registry.load(&package).unwrap();
    let contract = registry.resolve("U29").unwrap();
    // Conservatively stop projection/inclusion proof, without erasing runtime checks.
    assert_eq!(contract.shape(), Shape::Unknown);
    assert!(!contract.is_subtype_of(&registry.resolve("Text").unwrap()));
    assert!(contract.issues(&Data::Int(1)).is_empty());
    assert!(!contract.issues(&Data::Bool(false)).is_empty());
}
