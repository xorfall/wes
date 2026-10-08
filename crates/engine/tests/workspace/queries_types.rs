use super::*;

const PACKAGE: &str = r#"
types:
  Code: {base: Text, minLength: 1, pattern: '^[A-Z]+$'}
  SmallCode: {base: Code, maxLength: 3, pattern: '^A', enum: [AB, AC]}
  Price: {base: Decimal, min: 0.000000000000000000000000000001}
  BoundedPrice: {base: Price, max: 999999999999999999999999999999.5}
  Customer:
    base: Record
    fields:
      code: SmallCode
      price: BoundedPrice
      note: {type: Text, optional: true}
  Customers: {base: 'List<Customer>', minItems: 1, maxItems: 10}
"#;

fn load(workspace: &mut Workspace, yaml: &str) {
    let prepared = workspace.prepare_type_package(yaml, Span::at(0)).unwrap();
    workspace.commit(prepared).unwrap();
}
fn record(data: &Data) -> &indexmap::IndexMap<String, Data> {
    let Data::Record(record) = data else {
        panic!("record: {data:?}")
    };
    record
}
fn rows(data: &Data) -> &[Data] {
    let Data::List(rows) = data else {
        panic!("list: {data:?}")
    };
    rows
}
fn row<'a>(data: &'a Data, name: &str) -> &'a indexmap::IndexMap<String, Data> {
    rows(data)
        .iter()
        .map(record)
        .find(|r| r["name"] == Data::Text(name.into()))
        .unwrap()
}

#[tokio::test]
async fn discovery_describes_resolved_constraints_optional_fields_and_finite_constructors() {
    let (mut workspace, calls) = workspace();
    load(&mut workspace, PACKAGE);
    let sources = workspace.contracts().sources().to_vec();
    let count = workspace.contracts().snapshot().len();
    let listed = commit(&mut workspace, ":list types > types").unwrap();
    let customer = commit(&mut workspace, ":inspect type:Customer > customer").unwrap();
    let generic = commit(
        &mut workspace,
        r#":inspect type:"Map<Text, Option<Iter<Customer>>>" > generic"#,
    )
    .unwrap();
    let constructor = commit(&mut workspace, ":inspect type:Map > constructor").unwrap();
    let customers = commit(&mut workspace, ":inspect type:Customers > customers").unwrap();
    run_all(&mut workspace).await;
    let list = workspace.runtime().value_of(&listed).unwrap().data();
    assert_eq!(rows(list).len(), count + 6);
    assert_eq!(row(list, "Customer")["origin"], Data::Text("loaded".into()));
    assert_eq!(row(list, "Int")["origin"], Data::Text("builtin".into()));
    assert_eq!(
        row(list, "Map")["parameters"],
        Data::List(vec![Data::Text("Text".into()), Data::Text("T".into())])
    );
    assert_eq!(row(list, "Iter")["kind"], Data::Text("constructor".into()));
    assert_eq!(
        row(list, "Dataset")["parameters"],
        Data::List(vec![Data::Text("T".into())])
    );
    let description = fields(workspace.runtime().value_of(&customer).unwrap());
    let fs = &description["fields"];
    assert_eq!(row(fs, "note")["optional"], Data::Bool(true));
    assert_eq!(row(fs, "code")["optional"], Data::Bool(false));
    let code = record(&record(&row(fs, "code")["contract"])["constraints"]);
    assert_eq!(
        code["patterns"],
        Data::List(vec![Data::Text("^[A-Z]+$".into()), Data::Text("^A".into())])
    );
    assert_eq!(
        code["enum"],
        Data::List(vec![Data::Text("AB".into()), Data::Text("AC".into())])
    );
    assert_eq!(code["minLength"], Data::Int(1));
    assert_eq!(code["maxLength"], Data::Int(3));
    let price = record(&record(&row(fs, "price")["contract"])["constraints"]);
    for (key, expected) in [
        ("min", "0.000000000000000000000000000001"),
        ("max", "999999999999999999999999999999.5"),
    ] {
        let Data::Text(actual) = &price[key] else {
            panic!("lossless numeric text")
        };
        assert_eq!(
            actual.parse::<wes_core::Decimal>().unwrap(),
            expected.parse::<wes_core::Decimal>().unwrap()
        );
    }
    let collection = fields(workspace.runtime().value_of(&customers).unwrap());
    assert_eq!(
        record(&collection["constraints"])["maxItems"],
        Data::Int(10)
    );
    assert_eq!(
        record(&collection["element"])["name"],
        Data::Text("Customer".into())
    );
    let generic = fields(workspace.runtime().value_of(&generic).unwrap());
    assert_eq!(generic["kind"], Data::Text("map".into()));
    assert_eq!(record(&generic["key"])["name"], Data::Text("Text".into()));
    let iter = record(&record(&generic["value"])["element"]);
    assert_eq!(iter["kind"], Data::Text("iter".into()));
    assert_eq!(
        record(&iter["element"])["name"],
        Data::Text("Customer".into())
    );
    assert_eq!(
        fields(workspace.runtime().value_of(&constructor).unwrap())["parameters"],
        row(list, "Map")["parameters"]
    );
    assert_eq!(workspace.contracts().sources(), sources);
    assert_eq!(workspace.contracts().snapshot().len(), count);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn type_queries_capture_at_authorized_entry_and_refresh_observes_new_names() {
    let (mut workspace, calls) = workspace();
    let listed = commit(&mut workspace, ":list types > types").unwrap();
    let queued = ticket(workspace.start(Duration::ZERO));
    load(&mut workspace, "types: {Before: {base: Int}}");
    let entered = workspace.enter_ticket(queued).unwrap().unwrap();
    load(&mut workspace, "types: {After: {base: Text}}");
    finish(&mut workspace, entered).await;
    let list = workspace.runtime().value_of(&listed).unwrap().data();
    row(list, "Before");
    assert!(
        !rows(list)
            .iter()
            .any(|r| record(r)["name"] == Data::Text("After".into()))
    );
    let next = ticket(refresh(&mut workspace, &listed));
    execute(&mut workspace, next).await;
    row(
        workspace.runtime().value_of(&listed).unwrap().data(),
        "After",
    );

    // The absent name is not rejected at submission, but its absence is captured at entry.
    let inspected = commit(&mut workspace, ":inspect type:Later > later").unwrap();
    let queued = ticket(workspace.start(Duration::ZERO));
    let entered = workspace.enter_ticket(queued).unwrap().unwrap();
    load(&mut workspace, "types: {Later: {base: Bool}}");
    finish(&mut workspace, entered).await;
    assert_eq!(
        workspace.runtime().error_of(&inspected).unwrap().code(),
        "TYP003"
    );
    assert!(workspace.runtime().value_of(&inspected).is_none());
    let next = ticket(refresh(&mut workspace, &inspected));
    execute(&mut workspace, next).await;
    assert_eq!(
        fields(workspace.runtime().value_of(&inspected).unwrap())["primitive"],
        Data::Text("Bool".into())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_type_expressions_preserve_diagnostics_and_can_be_inspected_as_errors() {
    for expression in ["Missing", "List<Int", "Map<Int,Text>", "List<Int,Text>"] {
        let (mut workspace, calls) = workspace();
        let expected = workspace.contracts().resolve(expression).unwrap_err();
        let source = format!(r#":inspect type:"{expression}" > details *> failure"#);
        let failed = commit(&mut workspace, &source).unwrap();
        let handler = commit(&mut workspace, ":read $failure > problem").unwrap();
        run_all(&mut workspace).await;
        let actual = workspace.runtime().error_of(&failed).unwrap();
        assert_eq!(actual.code(), expected.code);
        assert_eq!(actual.message(), expected.message);
        assert!(workspace.runtime().value_of(&failed).is_none());
        assert_eq!(
            workspace.runtime().value_of(&handler),
            Some(&actual.to_value())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn provider_named_type_keeps_its_capability_namespace() {
    let (mut workspace, calls) = workspace();
    workspace
        .register_provider(
            ProviderDescription::new(
                "type",
                [Capability::new(
                    ["Customer"],
                    Shape::Primitive(Primitive::Int),
                    Safety::Safe,
                )],
                vec![],
            )
            .unwrap(),
            Arc::new(Echo(calls.clone())),
        )
        .unwrap();
    load(&mut workspace, PACKAGE);
    let node = commit(
        &mut workspace,
        ":inspect capability:\"type Customer\" > capability",
    )
    .unwrap();
    run_all(&mut workspace).await;
    assert_eq!(
        invocation(workspace.runtime().value_of(&node).unwrap())["capability"],
        Data::Text("Customer".into())
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn union_inspection_retains_alternatives_and_their_constraints() {
    let (mut workspace, _) = workspace();
    load(&mut workspace, PACKAGE);
    let node = commit(
        &mut workspace,
        r#":inspect type:"Union<SmallCode, Int>" > contract"#,
    )
    .unwrap();
    run_all(&mut workspace).await;
    let description = fields(workspace.runtime().value_of(&node).unwrap());
    assert_eq!(description["kind"], Data::Text("union".into()));
    let alternatives = rows(&description["alternatives"]);
    assert_eq!(
        record(&alternatives[0])["name"],
        Data::Text("SmallCode".into())
    );
    assert_eq!(
        record(&record(&alternatives[0])["constraints"])["maxLength"],
        Data::Int(3)
    );
    assert_eq!(
        record(&alternatives[1])["primitive"],
        Data::Text("Int".into())
    );
}

#[tokio::test]
async fn type_inspection_describes_display_separately_from_validation_constraints() {
    let (mut workspace, calls) = workspace();
    load(
        &mut workspace,
        "version: 2\ntypes: {Status: {base: Text, enum: [ready, failed], display: {enumTones: {ready: ok, failed: bad}}}} ",
    );
    let node = commit(&mut workspace, ":inspect type:Status > description").unwrap();
    run_all(&mut workspace).await;
    let description = fields(workspace.runtime().value_of(&node).unwrap());
    assert_eq!(
        record(&record(&description["display"])["enumTones"])["ready"],
        Data::Text("ok".into())
    );
    assert!(
        record(&description["constraints"])
            .get("enumTones")
            .is_none()
    );
    assert_eq!(
        description["digest"],
        Data::Text(
            workspace
                .contracts()
                .resolve("Status")
                .unwrap()
                .digest()
                .into()
        )
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
