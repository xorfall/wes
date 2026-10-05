use indexmap::IndexMap;
use std::sync::Arc;
use wes_core::{
    Data, Provenance, Shape, Value, capability::Catalogue, contracts::ContractRegistry,
};
use wes_engine::{
    calc::{Failure, Limits, Machine, Step},
    driver::CancellationToken,
    graph::{NodeId, OutputRef},
    plan::Input,
};
use wes_language::calc::{self, Package};

fn source() -> Value {
    Value::new(
        Shape::Unknown,
        Data::Record(IndexMap::from([
            ("a".into(), Data::Text("x".repeat(10_000).into())),
            ("bytes".into(), Data::Bytes(vec![42; 10_000].into())),
            ("b".into(), Data::Int(7)),
        ])),
        Provenance::default().with_fact("fixture", "synthetic-shared-payload"),
    )
    .unwrap()
}

fn evaluate(body: &str, source: &Value, limits: Limits) -> Result<Value, Failure> {
    let program = calc::parse_body(body, 0, Package::standard()).unwrap();
    let compiled = calc::analyze(
        Arc::new(program),
        calc::Environment {
            catalogue: &Catalogue::new(),
            contracts: &ContractRegistry::new(),
            workspace: &|name| (name == "source").then(|| source.shape().clone()),
        },
    )
    .unwrap();
    let mut machine = Machine::new(
        Arc::new(compiled),
        IndexMap::from([("source".into(), source.clone())]),
        limits,
    )?;
    let token = CancellationToken::new();
    loop {
        match machine.poll(&token)? {
            Step::Complete(value) => return Ok(value),
            Step::Yield => {}
            Step::Request(_) => panic!("local projection requested external work"),
        }
    }
}

fn fields(value: &Value) -> &IndexMap<String, Data> {
    let Data::Record(fields) = value.data() else {
        panic!("expected record")
    };
    fields
}

fn same_payload(left: &Data, right: &Data) {
    match (left, right) {
        (Data::Text(a), Data::Text(b)) => assert!(Arc::ptr_eq(a, b)),
        (Data::Bytes(a), Data::Bytes(b)) => assert!(Arc::ptr_eq(a, b)),
        _ => panic!("expected matching payload kinds"),
    }
}

#[test]
fn unchanged_payloads_share_backing_storage_through_calc_and_field_inputs() {
    let source = source();
    let projected = evaluate(
        "return {a: $source.a, again: $source.a, bytes: $source.bytes};",
        &source,
        Limits::default(),
    )
    .unwrap();
    assert_eq!(fields(&projected).len(), 3);
    assert!(!fields(&projected).contains_key("b"));
    assert_eq!(projected.provenance(), source.provenance());
    same_payload(&fields(&source)["a"], &fields(&projected)["a"]);
    same_payload(&fields(&source)["a"], &fields(&projected)["again"]);
    same_payload(&fields(&source)["bytes"], &fields(&projected)["bytes"]);

    // Repeated stage-like round trips preserve the original allocation, not merely content.
    let mut current = projected;
    for _ in 0..20 {
        current = evaluate(
            "return {a: $source.a, bytes: $source.bytes};",
            &current,
            Limits::default(),
        )
        .unwrap();
        same_payload(&fields(&source)["a"], &fields(&current)["a"]);
        same_payload(&fields(&source)["bytes"], &fields(&current)["bytes"]);
    }
    let node = NodeId::new("fixture").unwrap();
    let inputs = IndexMap::from([(node.clone(), current)]);
    for field in ["a", "bytes"] {
        let input = Input::FieldPath {
            output: OutputRef::data(node.clone()),
            fields: vec![field.into()],
        };
        let selected = input.resolve(&inputs).unwrap().into_owned();
        same_payload(&fields(&source)[field], selected.data());
        assert_eq!(selected.provenance(), source.provenance());
    }
}

#[test]
fn changed_text_has_independent_storage_and_projected_fields_stay_absent() {
    let source = source();
    let changed = evaluate("return {a: $source.a + '!'};", &source, Limits::default()).unwrap();
    let (Data::Text(before), Data::Text(after)) = (&fields(&source)["a"], &fields(&changed)["a"])
    else {
        panic!()
    };
    assert_eq!(before.len(), 10_000);
    assert_eq!(after.len(), 10_001);
    assert!(!Arc::ptr_eq(before, after));
    assert!(after.ends_with('!'));
    let node = NodeId::new("projected").unwrap();
    let field = Input::FieldPath {
        output: OutputRef::data(node.clone()),
        fields: vec!["b".into()],
    };
    assert!(field.resolve(&IndexMap::from([(node, changed)])).is_err());
}

#[test]
fn shared_payloads_still_obey_logical_calculation_budgets() {
    let error = evaluate(
        "return $source;",
        &source(),
        Limits {
            bytes: 1024,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "CAL006");
}
