use wes_core::contracts::ContractRegistry;
use wes_language::{Call, Expression, SourceText, Template, Value, parse, templates::Templates};

fn expression(text: &str) -> Expression {
    let parsed = parse(&SourceText::new("test", text));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.script.statements.len(), 1);
    parsed
        .script
        .statements
        .into_iter()
        .next()
        .unwrap()
        .expression
}
fn definition(text: &str) -> Template {
    let Expression::Definition(template) = expression(text) else {
        panic!("definition")
    };
    template
}
fn call(text: &str) -> Call {
    let Expression::Call(call) = expression(text) else {
        panic!("call")
    };
    call
}
fn types() -> ContractRegistry {
    let mut registry = ContractRegistry::new();
    registry.load("types:\n Category: {base: Text, enum: [books, games]}\n Books: {base: Category, enum: [books]}\n").unwrap();
    registry
}

#[test]
fn nested_templates_compose_guards_by_destination_argument() {
    let types = types();
    let mut templates = Templates::new();
    templates
        .define(
            definition(":def products(category: Category) as catalog list category:?category"),
            &types,
        )
        .unwrap();
    templates
        .define(
            definition(":def books(section: Books) as products category:?section"),
            &types,
        )
        .unwrap();
    let expanded = templates
        .expand(&call("books section:$chosen"), |_| false)
        .unwrap();
    assert_eq!(expanded.call.to_string(), "catalog list category:$chosen");
    assert_eq!(
        expanded.contracts["category"]
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>(),
        ["Books", "Category"]
    );
    assert!(matches!(
        expanded.call.arguments[0].value,
        Value::Reference(_)
    ));
}

#[test]
fn one_guarded_use_does_not_refine_other_uses_of_the_same_reference() {
    let types = types();
    let mut templates = Templates::new();
    templates
        .define(
            definition(":def both(a: Books) as catalog pair a:?a b:?b"),
            &types,
        )
        .unwrap();
    let expanded = templates
        .expand(&call("both a:$same b:$same"), |_| false)
        .unwrap();
    assert!(expanded.contracts.contains_key("a"));
    assert!(!expanded.contracts.contains_key("b"));
    assert_eq!(
        expanded.call.arguments[0].value.name().text,
        expanded.call.arguments[1].value.name().text
    );
}

#[test]
fn broad_outer_guard_cannot_erase_inner_constraint() {
    let types = types();
    let mut templates = Templates::new();
    templates
        .define(
            definition(":def inner(x: Int) as catalog echo value:?x"),
            &types,
        )
        .unwrap();
    templates
        .define(definition(":def outer(x: Unknown) as inner x:?x"), &types)
        .unwrap();
    let expanded = templates.expand(&call("outer x:3"), |_| false).unwrap();
    assert_eq!(
        expanded.contracts["value"]
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>(),
        ["Unknown", "Int"]
    );
}

#[test]
fn references_remain_caller_references_and_text_is_never_reparsed() {
    let types = types();
    let mut templates = Templates::new();
    templates
        .define(
            definition(":def pair as catalog pair a:?item b:$current"),
            &types,
        )
        .unwrap();
    let invocation = call("pair item:\"text > injected\"");
    let expanded = templates.expand(&invocation, |_| false).unwrap();
    assert!(
        matches!(&expanded.call.arguments[0].value,Value::Text(n) if n.text == "text > injected")
    );
    assert!(matches!(&expanded.call.arguments[1].value,Value::Reference(n) if n.text == "current"));
    assert_eq!(expanded.call.span, invocation.span);
    assert_eq!(
        expanded.call.arguments[0].value.name().span,
        invocation.arguments[0].value.name().span
    );
    templates
        .define(
            definition(":def literal as catalog echo value:\"?item\""),
            &types,
        )
        .unwrap();
    assert!(templates.snapshot()["literal"].parameters.is_empty());
    assert_eq!(
        templates
            .expand(&call("literal"), |_| false)
            .unwrap()
            .call
            .to_string(),
        "catalog echo value:\"?item\""
    );
}

#[test]
fn failed_definitions_and_forward_cycles_do_not_change_the_registry() {
    let types = types();
    let mut templates = Templates::new();
    templates
        .define(definition(":def first as second value:?value"), &types)
        .unwrap();
    assert_eq!(
        templates
            .define(definition(":def second as first value:?value"), &types)
            .unwrap_err()
            .code,
        "TMP005"
    );
    assert!(!templates.contains("second"));
    assert_eq!(
        templates
            .define(
                definition(":def first as catalog echo value:?value"),
                &types
            )
            .unwrap_err()
            .code,
        "TMP002"
    );
    templates
        .define(
            definition(":def second as catalog echo value:?value"),
            &types,
        )
        .unwrap();
    assert_eq!(
        templates
            .expand(&call("first value:ok"), |_| false)
            .unwrap()
            .call
            .to_string(),
        "catalog echo value:ok"
    );
}

#[test]
fn template_calls_reject_missing_unknown_duplicate_and_positional_arguments() {
    let types = types();
    let mut templates = Templates::new();
    templates
        .define(
            definition(":def products as catalog list category:?category"),
            &types,
        )
        .unwrap();
    for input in [
        "products",
        "products category:books extra:x",
        "products category:books category:games",
        "products books",
        "products $x",
    ] {
        assert_eq!(
            templates.expand(&call(input), |_| false).unwrap_err().code,
            "TMP003",
            "{input}"
        );
    }
}

#[test]
fn invalid_body_parameter_and_type_declarations_fail_before_installation() {
    for input in [
        ":def name as :refresh $node",
        ":def name as refresh $node",
        ":def name(x: Text) as catalog echo value:fixed",
        ":def name as ?provider read value:?x",
        ":def name as catalog echo value:?",
        ":def name as catalog echo value:?x value:other",
        ":def name(x: Missing) as catalog echo value:?x",
        ":def name(x: Text, x: Int) as catalog echo value:?x",
        ":def name as name value:?x",
    ] {
        let mut templates = Templates::new();
        assert!(
            templates.define(definition(input), &types()).is_err(),
            "{input}"
        );
        assert!(templates.snapshot().is_empty());
    }
}

#[test]
fn ambiguity_is_explicit_and_a_meta_marker_bypasses_templates() {
    let mut templates = Templates::new();
    templates
        .define(definition(":def help as catalog echo value:ok"), &types())
        .unwrap();
    assert_eq!(
        templates
            .expand(&call("help"), |name| name == "help")
            .unwrap_err()
            .code,
        "TMP002"
    );
    assert_eq!(
        templates
            .expand(&call(":help"), |_| true)
            .unwrap()
            .call
            .to_string(),
        ":help"
    );
}

#[test]
fn unused_declared_types_and_implicit_parameters_are_distinct() {
    let mut templates = Templates::new();
    templates
        .define(
            definition(":def partly(a: Books) as catalog pair a:?a b:?b"),
            &types(),
        )
        .unwrap();
    assert_eq!(
        templates.snapshot()["partly"]
            .parameters
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(templates.snapshot()["partly"].contracts.len(), 1);
}

#[test]
fn template_depth_and_argument_budgets_are_bounded() {
    let mut templates = Templates::new();
    let types = types();
    templates
        .define(definition(":def t0 as catalog echo value:?v"), &types)
        .unwrap();
    for i in 1..64 {
        templates
            .define(definition(&format!(":def t{i} as t{} v:?v", i - 1)), &types)
            .unwrap();
    }
    assert_eq!(
        templates
            .define(definition(":def t64 as t63 v:?v"), &types)
            .unwrap_err()
            .code,
        "TMP005"
    );
    assert_eq!(
        templates
            .expand(&call("t63 v:ok"), |_| false)
            .unwrap()
            .call
            .to_string(),
        "catalog echo value:ok"
    );
    let too_many = (0..257)
        .map(|i| format!("a{i}:?p{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    assert_eq!(
        templates
            .define(
                definition(&format!(":def many as catalog echo {too_many}")),
                &types
            )
            .unwrap_err()
            .code,
        "TMP005"
    );
}

#[test]
fn calc_definitions_roundtrip_and_preserve_nested_guards() {
    let types = types();
    let mut templates = Templates::new();
    let source = ":def labels(input: List<Text>) -> List<Text> as :calc pure { return input; }";
    let parsed = parse(&SourceText::new("test", source));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let formatted = parsed.script.to_string();
    assert_eq!(formatted, source);
    assert!(
        parse(&SourceText::new("again", formatted))
            .diagnostics
            .is_empty()
    );
    templates
        .define_calculation(
            definition(":def selected(input: Category) -> Text as :calc pure { return input; }"),
            &types,
            &Default::default(),
            wes_language::calc::Package::standard(),
        )
        .unwrap();
    templates
        .define(
            definition(":def books(section: Books) as selected input:?section"),
            &types,
        )
        .unwrap();
    let expanded = templates
        .expand(&call("books section:$value"), |_| false)
        .unwrap();
    assert!(expanded.calculation.unwrap().conversion_eligible());
    assert_eq!(
        expanded.contracts["input"]
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>(),
        ["Books", "Category"]
    );
}

#[test]
fn calc_definitions_require_explicit_signature_and_no_workspace_capture() {
    for source in [
        ":def bad(input: Int, input: Text) -> Int as :calc { return input; }",
        ":def bad(input-rate: Int) -> Int as :calc { return 1; }",
        ":def bad(input: Int) -> Int as :calc { return $secret; }",
    ] {
        let mut templates = Templates::new();
        assert!(
            templates
                .define_calculation(
                    definition(source),
                    &types(),
                    &Default::default(),
                    wes_language::calc::Package::standard()
                )
                .is_err(),
            "{source}"
        );
        assert!(templates.snapshot().is_empty());
    }
    let parsed = parse(&SourceText::new(
        "bad",
        ":def bad(input: Int) -> Int as :calc { return ; }",
    ));
    assert!(!parsed.diagnostics.is_empty());
}
