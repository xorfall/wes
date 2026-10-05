use wes_language::{Expression, SourceText, Structure, Value, parse};

#[test]
fn structures_preserve_nested_references_quoted_keys_and_round_trip() {
    let text = "service update body:{mode:$level, \"x-id\":\"literal,$text\", nested:[$cfg.count, {reason:$job::error.message}]} ids:[one, two] > result";
    let parsed = parse(&SourceText::new("structured", text));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let Expression::Call(call) = &parsed.script.statements[0].expression else {
        panic!("call")
    };
    assert!(matches!(
        &call.arguments[0].value,
        Value::Structured(_, Structure::Record(_))
    ));
    assert!(matches!(
        &call.arguments[1].value,
        Value::Structured(_, Structure::List(_))
    ));
    assert_eq!(
        call.arguments[0]
            .value
            .references()
            .iter()
            .map(|n| n.text.as_str())
            .collect::<Vec<_>>(),
        ["level", "cfg.count", "job::error.message"]
    );
    for reference in call.arguments[0].value.references() {
        assert_eq!(
            &text[reference.span.start()..reference.span.end()],
            format!("${}", reference.text)
        );
    }
    let canonical = parsed.script.to_string();
    let reparsed = parse(&SourceText::new("canonical", &canonical));
    assert!(
        reparsed.diagnostics.is_empty(),
        "{:?}",
        reparsed.diagnostics
    );
    assert_eq!(reparsed.script.to_string(), canonical);
}

#[test]
fn comments_and_multiline_data_do_not_create_commands_or_hide_the_next_statement() {
    let text = "service update body:{\n mode:ready, // synthetic comment\n nested:[\"http://127.0.0.1:1/a\", {label:\"} ] //\"}]\n}\nservice read";
    let parsed = parse(&SourceText::new("structured", text));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.script.statements.len(), 2);
}

#[test]
fn malformed_duplicate_and_effectful_structures_fail_without_executable_statements() {
    for text in [
        "{x:one,x:two}",
        "{x:[one}",
        "{x:one y:two}",
        "{x::calc {return 1;}}",
        "{x:?parameter}",
        "{$key:one}",
        "{x:$source.}",
    ] {
        let parsed = parse(&SourceText::new(
            "structured",
            format!("service update body:{text}"),
        ));
        assert!(!parsed.diagnostics.is_empty(), "{text}");
    }
}

#[test]
fn depth_node_and_byte_limits_refuse_before_planning() {
    for body in [
        format!("{}one{}", "[".repeat(33), "]".repeat(33)),
        format!("[{}]", vec!["one"; 1001].join(",")),
        format!("{{x:\"{}\"}}", "x".repeat(65 * 1024)),
    ] {
        let parsed = parse(&SourceText::new(
            "structured",
            format!("service update body:{body}"),
        ));
        assert!(parsed.diagnostics.iter().any(|d| d.code == "ARG001"));
    }
    let parsed = parse(&SourceText::new(
        "quoted",
        "service update body:\"[one,two]\"",
    ));
    assert!(parsed.diagnostics.is_empty());
    let Expression::Call(call) = &parsed.script.statements[0].expression else {
        panic!("call")
    };
    assert!(matches!(call.arguments[0].value, Value::Text(_)));
}

#[test]
fn management_and_read_selection_refuse_structures_in_literal_only_slots() {
    for text in [
        ":env rename \"synthetic\" to:{a:one}",
        ":env rename \"synthetic\" to:[$secret]",
    ] {
        let parsed = parse(&SourceText::new("environment", text));
        assert!(parsed.diagnostics.is_empty());
        assert_eq!(
            wes_language::vocabulary::EnvironmentCommand::validate(&parsed.script.statements[0])
                .err()
                .expect("structure refused")
                .code,
            "ENV001"
        );
    }
    let parsed = parse(&SourceText::new(
        "read",
        ":read $source limit:{nested:[$other]}",
    ));
    let Expression::Call(call) = &parsed.script.statements[0].expression else {
        panic!("call")
    };
    assert!(wes_language::vocabulary::commands::invocation(call).is_err());
    let parsed = parse(&SourceText::new(
        "comment",
        "service update body:{note:a//b, mode:ready}",
    ));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
}

#[test]
fn comments_after_closed_values_ignore_delimiters_in_comment_text() {
    for value in ["\"x\"", "[one]", "{nested:one}"] {
        let parsed = parse(&SourceText::new(
            "comments",
            format!("service update body:{{a:{value}// comment with }} ]\n, b:two}}\nservice read"),
        ));
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_eq!(parsed.script.statements.len(), 2);
    }
}
