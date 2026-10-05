use indexmap::IndexMap;
use std::sync::Arc;
use wes_core::{
    Primitive, Provenance, Shape,
    capability::{
        Capability, Catalogue, DeclaredRule, Parameter, ProviderDescription, Rule, RuleBasis,
        Safety, Typing,
    },
};
use wes_language::{
    Expression, Severity, SourceText, Span,
    check::{self, Environment, ExistingCall, Given, GivenValue},
    parse,
    resolve::resolve,
};

fn fixtures() -> (Catalogue, Environment) {
    let mut search = Capability::new(["search"], Shape::Unknown, Safety::Safe);
    search.parameters = vec![
        Parameter::new("query", Shape::Primitive(Primitive::Text), true),
        Parameter::new("limit", Shape::Primitive(Primitive::Int), false),
        Parameter::new("cursor", Shape::Primitive(Primitive::Text), false),
        Parameter::new("page", Shape::Primitive(Primitive::Int), false),
        Parameter::new("data", Shape::Unknown, false),
    ];
    search.rules = vec![
        Rule::MutuallyExclusive(["cursor".into(), "page".into()].into()),
        Rule::Requires {
            key: "page".into(),
            needs: "limit".into(),
        },
        Rule::OneOf {
            key: "query".into(),
            values: ["books".into(), "games".into()].into(),
        },
        Rule::ProvenanceFact {
            key: "data".into(),
            fact: "verified".into(),
            expected: "true".into(),
        },
    ]
    .into_iter()
    .map(|rule| DeclaredRule {
        rule,
        basis: RuleBasis::Documented { note: None },
    })
    .collect();
    let mut suggest = search.clone();
    suggest.path = vec!["suggest".into()];
    suggest.rules = vec![DeclaredRule {
        rule: Rule::OneOf {
            key: "query".into(),
            values: ["books".into()].into(),
        },
        basis: RuleBasis::Inferred {
            reason: "example in documentation".into(),
        },
    }];
    let mut catalogue = Catalogue::new();
    catalogue.register(ProviderDescription::new("catalog", [search, suggest], vec![]).unwrap());
    let environment = Environment {
        bindings: IndexMap::from([
            (
                "text".into(),
                Typing::new(Shape::Primitive(Primitive::Text)),
            ),
            (
                "number".into(),
                Typing::new(Shape::Primitive(Primitive::Int)),
            ),
            (
                "good".into(),
                Typing {
                    shape: Shape::Unknown,
                    provenance: Provenance::default().with_fact("verified", "true"),
                },
            ),
            ("bad".into(), Typing::new(Shape::Unknown)),
        ]),
        ..Environment::default()
    };
    (catalogue, environment)
}
fn inspect(
    input: &str,
    catalogue: &Catalogue,
    environment: &Environment,
    validated: &IndexMap<String, Shape>,
) -> Vec<wes_language::Diagnostic> {
    let parsed = parse(&SourceText::new("test", input));
    let mut diagnostics = parsed.diagnostics;
    for statement in parsed.script.statements {
        let Expression::Call(call) = statement.expression else {
            panic!("call")
        };
        match resolve(&call, catalogue) {
            Ok(resolution) => diagnostics.extend(check::check(
                &resolution,
                &statement.annotations,
                validated,
                environment,
            )),
            Err(diagnostic) => diagnostics.push(diagnostic),
        }
    }
    diagnostics
}

#[test]
fn analysis_codes_severities_and_utf16_spans_match_acceptance_fixtures() {
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/analysis-acceptance.json"
    ))
    .unwrap();
    let (catalogue, environment) = fixtures();
    for case in cases.as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let source = SourceText::new("test", input);
        let diagnostics = inspect(input, &catalogue, &environment, &IndexMap::new());
        let actual=diagnostics.iter().map(|d|serde_json::json!({
            "code":d.code,"severity":match d.severity {Severity::Error=>"ERROR",Severity::Warning=>"WARNING",Severity::Info=>"INFO"},
            "start":source.utf16_offset(d.span.start()).unwrap(),"end":source.utf16_offset(d.span.end()).unwrap()
        })).collect::<Vec<_>>();
        assert_eq!(
            serde_json::Value::Array(actual),
            case["diagnostics"],
            "{input}"
        );
    }
}

#[test]
fn guard_shapes_are_argument_local_and_do_not_mutate_bindings() {
    let (mut catalogue, environment) = fixtures();
    let mut pair = Capability::new(["read"], Shape::Unknown, Safety::Safe);
    pair.parameters = vec![
        Parameter::new("a", Shape::Primitive(Primitive::Int), true),
        Parameter::new("b", Shape::Primitive(Primitive::Int), true),
    ];
    catalogue.register(ProviderDescription::new("pair", [pair], vec![]).unwrap());
    let guards = IndexMap::from([("a".into(), Shape::Primitive(Primitive::Int))]);
    let errors = inspect("pair read a:$bad b:$bad", &catalogue, &environment, &guards);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code, "CHK004");
    assert!(errors[0].message.contains("'b:'"));
    let public = errors[0].public_message.as_ref().unwrap();
    assert!(public.contains("'b:' requires Int"));
    assert!(public.contains(":type check"));
    assert!(!public.contains("Unknown"));
    assert_eq!(environment.bindings["bad"].shape, Shape::Unknown);
}

#[test]
fn change_rechecks_merged_call_and_preserves_previous_overrides() {
    let (catalogue, mut environment) = fixtures();
    let span = Span::at(0);
    let capability = catalogue
        .resolve(&["catalog".into(), "search".into()])
        .unwrap()
        .clone();
    let original = ExistingCall {
        capability,
        arguments: IndexMap::from([(
            "query".into(),
            Given {
                value: GivenValue::Written("music".into()),
                key: span,
                span,
            },
        )]),
        excused: ["query".into()].into(),
        validated_inputs: IndexMap::new(),
    };
    environment.calls.insert("result".into(), original);
    let diagnostics = inspect(
        ":change $result limit:2",
        &catalogue,
        &environment,
        &IndexMap::new(),
    );
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity, Severity::Warning);
    assert_eq!(
        diagnostics[0].public_summary(),
        "Argument is outside the declared choices; choice details are withheld."
    );
    assert!(!diagnostics[0].public_summary().contains("books"));
    assert_eq!(diagnostics[0].code, "CHK008");
    let diagnostics = inspect(
        ":change $result extra:2",
        &catalogue,
        &environment,
        &IndexMap::new(),
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == "CHK001" && d.severity == Severity::Error)
    );
    assert_eq!(environment.calls["result"].arguments.len(), 1);
}

#[test]
fn change_does_not_erase_guarded_reference_shape_or_provenance() {
    let (catalogue, mut environment) = fixtures();
    let span = Span::at(0);
    let capability = catalogue
        .resolve(&["catalog".into(), "search".into()])
        .unwrap()
        .clone();
    environment.calls.insert(
        "result".into(),
        ExistingCall {
            capability: Arc::clone(&capability),
            arguments: IndexMap::from([(
                "query".into(),
                Given {
                    value: GivenValue::Written("books".into()),
                    key: span,
                    span,
                },
            )]),
            excused: Default::default(),
            validated_inputs: IndexMap::from([("limit".into(), Shape::Primitive(Primitive::Int))]),
        },
    );
    assert!(
        inspect(
            ":change $result limit:$bad",
            &catalogue,
            &environment,
            &IndexMap::new()
        )
        .is_empty()
    );
    // This acceptance is conditional on a mandatory runtime guard; no global retyping occurred.
    assert_eq!(environment.bindings["bad"].shape, Shape::Unknown);
    assert!(
        inspect(
            ":change $result data:$bad",
            &catalogue,
            &environment,
            &IndexMap::new()
        )
        .iter()
        .any(|d| d.code == "CHK009")
    );
}

#[test]
fn inferred_rules_explain_their_basis_and_unchecked_does_not_disable_type_checks() {
    let (catalogue, environment) = fixtures();
    let diagnostics = inspect(
        "catalog suggest query:music",
        &catalogue,
        &environment,
        &IndexMap::new(),
    );
    assert_eq!(diagnostics[0].severity, Severity::Warning);
    assert_eq!(
        diagnostics[0].public_summary(),
        "Argument is outside the declared choices; choice details are withheld."
    );
    assert!(!diagnostics[0].public_summary().contains("books"));
    assert!(
        diagnostics[0]
            .hints
            .iter()
            .any(|h| h == "inferred from: example in documentation")
    );
    let diagnostics = inspect(
        "@unchecked{limit} catalog search query:books limit:bad",
        &catalogue,
        &environment,
        &IndexMap::new(),
    );
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == "CHK004" && d.severity == Severity::Error)
    );
}

#[test]
fn type_discovery_uses_literal_selectors_without_guessing_the_inspection_target() {
    let (mut catalogue, environment) = fixtures();
    catalogue.register(
        ProviderDescription::new(
            "type",
            [Capability::new(["Customer"], Shape::Unknown, Safety::Safe)],
            vec![],
        )
        .unwrap(),
    );
    for source in [
        ":list types",
        ":inspect type:Customer",
        r#":inspect type:"Map<Text, List<Customer>>""#,
        ":inspect type:List",
        ":inspect capability:\"type Customer\"",
        ":inspect catalog search",
        ":inspect $text",
    ] {
        let diagnostics = inspect(source, &catalogue, &environment, &IndexMap::new());
        assert!(diagnostics.is_empty(), "{source}: {diagnostics:?}");
    }
    for (source, code) in [
        (":inspect type:$text", "CMD002"),
        (":inspect type:Text $text", "CMD002"),
        (":inspect type:Text catalog search", "CMD002"),
        (":inspect catalog search $text", "CMD002"),
        (":inspect", "CMD002"),
        (":inspect type:Text type:Int", "CMD002"),
        (":inspect types:Text", "CMD002"),
        (":list types provider:catalog", "CHK001"),
    ] {
        let diagnostics = inspect(source, &catalogue, &environment, &IndexMap::new());
        assert!(
            diagnostics.iter().any(|d| d.code == code),
            "{source}: {diagnostics:?}"
        );
    }
}

#[test]
fn downstream_refresh_scope_is_optional_literal_selector_metadata() {
    use wes_language::vocabulary::{MetaCommand, RefreshScope};
    let spec = MetaCommand::Refresh.spec(&[]);
    let scope = spec.parameter("scope").unwrap();
    assert!(!scope.required);
    assert_eq!(
        scope.sort,
        wes_core::capability::Sort::Selector("refresh-scope".into())
    );
    assert!(!spec.recorded);
    assert_eq!(
        RefreshScope::lookup("downstream"),
        Some(RefreshScope::Downstream)
    );
    for absent in ["self", "force", "all"] {
        assert_eq!(RefreshScope::lookup(absent), None);
    }
    let (catalogue, environment) = fixtures();
    for source in [":refresh $text", ":refresh $text scope:downstream"] {
        assert!(inspect(source, &catalogue, &environment, &IndexMap::new()).is_empty());
    }
    let diagnostics = inspect(
        ":refresh $text scope:$text",
        &catalogue,
        &environment,
        &IndexMap::new(),
    );
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CHK017")
    );
}

#[test]
fn list_registry_domain_and_filter_contract_are_closed_and_discoverable() {
    use wes_language::vocabulary::{ListRegistry, MetaCommand};
    let (catalogue, environment) = fixtures();
    let names: std::collections::BTreeSet<_> = ListRegistry::ALL.iter().map(|r| r.name()).collect();
    assert_eq!(names.len(), ListRegistry::ALL.iter().count());
    assert_eq!(
        MetaCommand::List
            .spec(&[])
            .tail_words
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        names
    );
    for registry in ListRegistry::ALL.iter() {
        assert_eq!(ListRegistry::lookup(registry.name()), Some(*registry));
        let command = format!(":list {}", registry.name());
        assert!(
            inspect(&command, &catalogue, &environment, &IndexMap::new()).is_empty(),
            "{command}"
        );
        let filtered = inspect(
            &format!("{command} provider:catalog"),
            &catalogue,
            &environment,
            &IndexMap::new(),
        );
        if *registry == ListRegistry::Capabilities {
            assert!(filtered.is_empty(), "{filtered:?}");
        } else {
            assert!(
                filtered.iter().any(|d| d.code == "CHK001"),
                "{command}: {filtered:?}"
            );
        }
        let unknown = inspect(
            &format!("{command} unknown:ignored"),
            &catalogue,
            &environment,
            &IndexMap::new(),
        );
        assert!(unknown.iter().any(|d| d.code == "CHK001"));
    }
    assert!(
        inspect(
            ":list unregistered",
            &catalogue,
            &environment,
            &IndexMap::new()
        )
        .iter()
        .any(|d| d.code == "CHK015")
    );
    assert!(
        inspect(
            ":list capabilities provider:$text",
            &catalogue,
            &environment,
            &IndexMap::new()
        )
        .iter()
        .any(|d| d.code == "CHK017")
    );
}

#[test]
fn resource_suggestions_accept_typed_dependencies_without_relaxing_literal_selectors() {
    let (_, environment) = fixtures();
    let mut read = Capability::new(["read"], Shape::Unknown, Safety::Safe);
    read.parameters =
        vec![Parameter::new("id", Shape::Primitive(Primitive::Text), true).suggesting("resource")];
    let mut select = read.clone();
    select.path = vec!["select".into()];
    select.parameters[0] = select.parameters[0].clone().selecting("resource");
    let mut catalogue = Catalogue::new();
    catalogue.register(ProviderDescription::new("resources", [read, select], vec![]).unwrap());
    for source in ["resources read id:literal", "resources read id:$text"] {
        let diagnostics = inspect(source, &catalogue, &environment, &IndexMap::new());
        assert!(diagnostics.is_empty(), "{source}: {diagnostics:?}");
    }
    assert!(
        !inspect(
            "resources read id:$number",
            &catalogue,
            &environment,
            &IndexMap::new()
        )
        .is_empty()
    );
    assert!(
        inspect(
            "resources select id:$text",
            &catalogue,
            &environment,
            &IndexMap::new()
        )
        .iter()
        .any(|d| d.code == "CHK017")
    );
}
