use wes_core::{
    Data, Primitive, Shape,
    capability::{Capability, Catalogue, Parameter, ProviderDescription, Safety},
    contracts::ContractRegistry,
};
use wes_engine::{
    graph::{NodeId, OutputPort, OutputRef},
    plan::{self, Guards, Input, Plan, PlanError, Task},
};
use wes_language::{Expression, SourceText, parse, resolve::resolve, templates::Templates};

fn catalogue() -> Catalogue {
    let mut echo = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
    echo.parameters = vec![
        Parameter::new("value", Shape::Unknown, true),
        Parameter::new("other", Shape::Unknown, false),
    ];
    let mut count = Capability::new(["count"], Shape::Primitive(Primitive::Int), Safety::Safe);
    count.parameters = vec![Parameter::new(
        "amount",
        Shape::Primitive(Primitive::Int),
        true,
    )];
    let mut catalogue = Catalogue::new();
    catalogue.register(ProviderDescription::new("catalog", [echo, count], vec![]).unwrap());
    catalogue
}
fn names(name: &str) -> Option<OutputRef> {
    let port = match name {
        "source" => OutputPort::Data,
        "failure" | "source::error" => OutputPort::Error,
        "source::cancel" => OutputPort::Cancel,
        _ => return None,
    };
    Some(OutputRef {
        node: NodeId::new("id1000").unwrap(),
        port,
    })
}
fn make(input: &str) -> Result<Plan, PlanError> {
    let parsed = parse(&SourceText::new("test", input));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let statement = &parsed.script.statements[0];
    let Expression::Call(call) = &statement.expression else {
        panic!("call")
    };
    let resolution = resolve(call, &catalogue()).unwrap();
    plan::plan(&resolution, statement, &Guards::new(), &names)
}
fn rejection(error: PlanError) -> String {
    let PlanError::Rejected { diagnostic, .. } = error else {
        panic!("rejected")
    };
    diagnostic.code.into()
}

#[test]
fn predicted_provenance_joins_reference_facts_and_keeps_producer_overrides_and_cautions() {
    use wes_core::{Provenance, Value, capability::Typing};
    let Plan::NewNode(node) = make("@unchecked{value} catalog echo value:$source").unwrap() else {
        panic!("node")
    };
    let Task::Invoke(mut invoke) = node.task else {
        panic!("invoke")
    };
    let mut capability = (*invoke.capability).clone();
    capability.provenance_arguments.insert("locale".into());
    invoke.capability = std::sync::Arc::new(capability);
    invoke.inputs.insert(
        "locale".into(),
        Input::Literal(
            Value::new(
                Shape::Primitive(Primitive::Text),
                Data::Text("en".into()),
                Provenance::default(),
            )
            .unwrap(),
        ),
    );
    let source = Typing {
        shape: Shape::Unknown,
        provenance: Provenance::default()
            .with_fact("source", "catalog")
            .with_fact("locale", "old")
            .cautioned(["input-warning".into()]),
    };
    let predicted = invoke.predicted_typing(|_| Some(&source));
    assert_eq!(predicted.provenance.fact("source"), Some("catalog"));
    assert_eq!(predicted.provenance.fact("locale"), Some("en"));
    assert!(predicted.provenance.cautions().contains("unchecked:value"));
    assert!(predicted.provenance.cautions().contains("input-warning"));
    assert_eq!(predicted.shape, invoke.capability.result);
}

#[test]
fn absent_or_error_channel_origins_cannot_invent_predicted_consensus() {
    use wes_core::{Provenance, capability::Typing};
    let Plan::NewNode(node) = make("catalog echo value:$source").unwrap() else {
        panic!("node")
    };
    let Task::Invoke(mut invoke) = node.task else {
        panic!("invoke")
    };
    invoke.inputs.insert(
        "other".into(),
        Input::FromNode(OutputRef::data(NodeId::new("id1001").unwrap())),
    );
    let source = Typing {
        shape: Shape::Unknown,
        provenance: Provenance::default()
            .with_fact("source", "catalog")
            .cautioned(["input-warning".into()]),
    };
    let predicted = invoke.predicted_typing(|node| (node.as_str() == "id1000").then_some(&source));
    assert!(predicted.provenance.facts().is_empty());
    assert!(predicted.provenance.cautions().contains("input-warning"));
    invoke.inputs.shift_remove("other");
    invoke.inputs.insert(
        "failure".into(),
        Input::FromNode(OutputRef {
            node: NodeId::new("id1001").unwrap(),
            port: OutputPort::Error,
        }),
    );
    assert!(
        invoke
            .predicted_typing(|_| Some(&source))
            .provenance
            .facts()
            .is_empty()
    );
}

#[test]
fn arguments_are_either_contextual_values_or_captured_selected_outputs() {
    let Plan::NewNode(node) = make("catalog count amount:12 > total *> failure").unwrap() else {
        panic!("new node")
    };
    assert_eq!(node.name.as_deref(), Some("total"));
    assert_eq!(node.error_name.as_deref(), Some("failure"));
    assert!(node.dependencies.is_empty());
    let Task::Invoke(invoke) = node.task else {
        panic!("invoke")
    };
    assert!(
        matches!(&invoke.inputs["amount"],Input::Literal(value) if value.data()==&Data::Int(12))
    );
    let Plan::NewNode(node) = make("catalog echo value:$failure").unwrap() else {
        panic!("node")
    };
    assert_eq!(
        node.dependencies.values().copied().collect::<Vec<_>>(),
        [OutputPort::Error]
    );
}

#[test]
fn value_producing_meta_commands_have_dependencies_but_actions_are_not_bindable() {
    let Plan::NewNode(node) = make(":type check $source as:Text > shown").unwrap() else {
        panic!("node")
    };
    assert_eq!(node.dependencies.len(), 1);
    assert!(
        matches!(make(":refresh $source").unwrap(),Plan::Action{targets,..} if targets.len()==1)
    );
    assert_eq!(
        rejection(make(":refresh $source > result").unwrap_err()),
        "PLN003"
    );
    assert_eq!(
        rejection(make(":cancel $source *> failure").unwrap_err()),
        "PLN003"
    );
}

#[test]
fn contradictory_output_dependencies_fail_before_node_creation() {
    assert_eq!(
        rejection(make("catalog echo value:$source other:$source::error").unwrap_err()),
        "PLN002"
    );
    assert_eq!(
        rejection(make("catalog echo value:$missing").unwrap_err()),
        "PLN001"
    );
    assert_eq!(
        rejection(make("catalog count amount:invalid").unwrap_err()),
        "CHK004"
    );
}

#[test]
fn annotations_keep_execution_intent_separate_from_value_cautions() {
    let Plan::NewNode(node) =
        make("@unchecked{value} @interactive catalog echo value:hello").unwrap()
    else {
        panic!("node")
    };
    let Task::Invoke(invoke) = node.task else {
        panic!("invoke")
    };
    assert!(invoke.interactive);
    assert_eq!(
        invoke.cautions.into_iter().collect::<Vec<_>>(),
        ["unchecked:value"]
    );
}

#[test]
fn nested_template_guards_validate_literals_but_keep_references_for_runtime() {
    let mut types = ContractRegistry::new();
    types
        .load("types: {Positive: {base: Int, min: 1}}")
        .unwrap();
    let mut templates = Templates::new();
    for source in [
        ":def positive(value: Positive) as catalog echo value:?value",
        ":def outer(value: Unknown) as positive value:?value",
    ] {
        let Expression::Definition(definition) = parse(&SourceText::new("test", source))
            .script
            .statements
            .remove(0)
            .expression
        else {
            panic!("definition")
        };
        templates.define(definition, &types).unwrap();
    }
    let prepare = |source: &str| {
        let statement = parse(&SourceText::new("test", source))
            .script
            .statements
            .remove(0);
        let Expression::Call(call) = &statement.expression else {
            panic!("call")
        };
        let expanded = templates.expand(call, |_| false).unwrap();
        let resolution = resolve(&expanded.call, &catalogue()).unwrap();
        plan::plan(&resolution, &statement, &expanded.contracts, &names)
    };
    let Plan::NewNode(node) = prepare("outer value:3 > result").unwrap() else {
        panic!("node")
    };
    let Task::Invoke(invoke) = node.task else {
        panic!("invoke")
    };
    assert!(matches!(&invoke.inputs["value"],Input::Literal(value) if value.data()==&Data::Int(3)));
    assert_eq!(invoke.guards["value"].len(), 2);
    let PlanError::Rejected { diagnostic, issues } = prepare("outer value:-1").unwrap_err() else {
        panic!("validation")
    };
    assert_eq!(diagnostic.code, "TYP005");
    assert_eq!(issues[0].path, "/arguments/value");
    let Plan::NewNode(node) = prepare("outer value:$source").unwrap() else {
        panic!("node")
    };
    let Task::Invoke(invoke) = node.task else {
        panic!("invoke")
    };
    assert!(matches!(invoke.inputs["value"], Input::FromNode(_)));
    assert_eq!(invoke.guards["value"].len(), 2);
}

#[test]
fn nested_fields_select_ticket_data_and_preserve_root_provenance_and_privacy() {
    use wes_core::{Provenance, RecordShape, Value};
    let Plan::NewNode(node) = make("catalog echo value:$source.address.city").unwrap() else {
        panic!("node");
    };
    assert_eq!(
        node.dependencies[&NodeId::new("id1000").unwrap()],
        OutputPort::Data
    );
    let Task::Invoke(invoke) = node.task else {
        panic!("call");
    };
    let shape = Shape::Record(
        RecordShape::new(
            "Address",
            [("city".into(), Shape::Primitive(Primitive::Text))],
        )
        .unwrap(),
    );
    let shape = Shape::Record(RecordShape::new("Item", [("address".into(), shape)]).unwrap());
    let data = Data::Record(
        [(
            "address".into(),
            Data::Record([("city".into(), Data::Text("Ankara".into()))].into()),
        )]
        .into(),
    );
    let root = Value::new(
        shape,
        data,
        Provenance::default()
            .with_fact("source", "fixture")
            .with_policy(&wes_core::flow::FlowPolicy::default().private()),
    )
    .unwrap();
    let inputs = [(NodeId::new("id1000").unwrap(), root)].into();
    let selected = invoke.inputs["value"].resolve(&inputs).unwrap();
    assert_eq!(selected.data(), &Data::Text("Ankara".into()));
    assert_eq!(selected.shape(), &Shape::Primitive(Primitive::Text));
    assert_eq!(selected.provenance().fact("source"), Some("fixture"));
    assert!(selected.provenance().policy().is_private());
}

#[test]
fn field_paths_preserve_output_ports_and_cannot_be_used_as_control_identities() {
    let Plan::NewNode(node) = make("catalog echo value:$source::error.code").unwrap() else {
        panic!("node");
    };
    assert_eq!(
        node.dependencies[&NodeId::new("id1000").unwrap()],
        OutputPort::Error
    );
    assert_eq!(
        rejection(make("catalog echo value:$source.id other:$source::error.code").unwrap_err()),
        "PLN002"
    );
    assert_eq!(
        rejection(make(":refresh $source.id").unwrap_err()),
        "PLN004"
    );
}

#[test]
fn projection_errors_distinguish_guarantees_data_and_types_without_guessing() {
    use wes_core::{Provenance, RecordShape, Value};
    use wes_engine::plan::{InputProblem, project_value};
    let fields = vec!["id".to_string()];
    let data = Data::Record([("id".into(), Data::Text("independent sample".into()))].into());
    let generic = Shape::Record(RecordShape::new("Record", []).unwrap());
    let root = Value::new(generic, data.clone(), Provenance::default()).unwrap();
    let error = project_value(&root, &fields).unwrap_err();
    assert_eq!(error.problem, InputProblem::FieldNotGuaranteed);
    assert_eq!(error.issue("body").unwrap().code, "INP002");
    assert!(error.message().contains("Check the whole value"));
    let dynamic = Value::new(Shape::Unknown, data, Provenance::default()).unwrap();
    assert_eq!(
        project_value(&dynamic, &fields).unwrap().shape(),
        &Shape::Unknown
    );
    let absent = Value::new(
        Shape::Unknown,
        Data::Record(Default::default()),
        Provenance::default(),
    )
    .unwrap();
    assert_eq!(
        project_value(&absent, &fields).unwrap_err().problem,
        InputProblem::FieldMissing
    );
    let declared = Shape::Record(
        RecordShape::new("Row", [("id".into(), Shape::Primitive(Primitive::Text))]).unwrap(),
    );
    let malformed = Value::new(
        declared,
        Data::Record([("id".into(), Data::Int(1))].into()),
        Provenance::default(),
    )
    .unwrap();
    assert_eq!(
        project_value(&malformed, &fields).unwrap_err().problem,
        InputProblem::TypeMismatch
    );
    let nested_shape = Shape::Record(
        RecordShape::new(
            "Outer",
            [(
                "body".into(),
                Shape::Record(
                    RecordShape::new("Body", [("id".into(), Shape::Primitive(Primitive::Text))])
                        .unwrap(),
                ),
            )],
        )
        .unwrap(),
    );
    let nested_malformed = Value::new(
        nested_shape,
        Data::Record([("body".into(), Data::Text("not a record".into()))].into()),
        Provenance::default(),
    )
    .unwrap();
    assert_eq!(
        project_value(&nested_malformed, &["body".into(), "id".into()])
            .unwrap_err()
            .problem,
        InputProblem::TypeMismatch
    );
    assert!(project_value(&root, &[]).unwrap().data() == root.data());
}

#[test]
fn input_failures_keep_port_identity_and_escape_paths_but_redact_private_details() {
    use wes_core::{Provenance, Value};
    use wes_engine::{plan::InputProblem, runtime::RuntimeCode};
    let source = OutputRef {
        node: NodeId::new("synthetic").unwrap(),
        port: OutputPort::Error,
    };
    let input = Input::FieldPath {
        output: source.clone(),
        fields: vec!["a/b~c".into()],
    };
    let missing = input.resolve(&Default::default()).unwrap_err();
    assert_eq!(missing.problem, InputProblem::SourceUnavailable);
    assert_eq!(missing.source, Some(source.clone()));
    assert_eq!(
        missing.issue("a/b").unwrap().path,
        "/arguments/a~1b/a~1b~0c"
    );
    let root = Value::new(
        Shape::Unknown,
        Data::Record(Default::default()),
        Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
    )
    .unwrap();
    let inputs = [(source.node, root)].into();
    let error = input.resolve(&inputs).unwrap_err();
    assert!(error.issue("value").is_none());
    let failure = error.error(RuntimeCode::InputFailed, "value");
    assert!(failure.policy().is_private());
    assert_eq!(failure.code(), "ENV021");
    assert!(failure.issues().is_empty());
    assert!(!failure.message().contains("a/b~c"));
}

#[test]
fn structured_inputs_capture_every_nested_port_and_reject_conflicting_selections() {
    let Plan::NewNode(node) =
        make("catalog echo value:{nested:[$source, {code:$source}]} other:[]").unwrap()
    else {
        panic!("node")
    };
    assert_eq!(node.dependencies.len(), 1);
    assert_eq!(
        node.dependencies[&NodeId::new("id1000").unwrap()],
        OutputPort::Data
    );
    let Task::Invoke(invocation) = node.task else {
        panic!("invocation")
    };
    assert_eq!(invocation.inputs["value"].dependencies().len(), 2);
    assert_eq!(
        rejection(make("catalog echo value:{data:$source, errors:[$source::error]}").unwrap_err()),
        "PLN002"
    );
    assert_eq!(
        rejection(make("catalog echo value:{missing:[$unknown]}").unwrap_err()),
        "PLN001"
    );
}
