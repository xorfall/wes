use wes_language::{Expression, SourceText, TokenKind, lex, parse};

#[test]
fn pipes_group_stages_preserve_bindings_spans_and_roundtrip() {
    for text in [
        "first read > raw *> err | second write value:input.x > result *> failed",
        ":calc { return [1,2]; }\n| :calc { return input; } > result",
        "$source::error |\n :calc { return input.message; }",
        "first read|second write value:input|:calc { return input; }",
    ] {
        let parsed = parse(&SourceText::new("pipe", text));
        assert!(
            parsed.diagnostics.is_empty(),
            "{text}: {:?}",
            parsed.diagnostics
        );
        assert_eq!(parsed.script.statements.len(), 1);
        let statement = &parsed.script.statements[0];
        assert_eq!(&text[statement.span.start()..statement.span.end()], text);
        let Expression::Pipeline(stages) = &statement.expression else {
            panic!("pipeline");
        };
        assert!(stages.len() >= 2);
        for stage in stages {
            assert!(!text[stage.span.start()..stage.span.end()].contains("|"));
        }
        let written = parsed.script.to_string();
        let roundtrip = parse(&SourceText::new("printed", &written));
        assert!(roundtrip.diagnostics.is_empty());
        assert_eq!(roundtrip.script.to_string(), written);
    }
}

#[test]
fn shell_strings_calc_strings_and_boolean_operators_do_not_create_stages() {
    let text = "sh run cmd:\"ls -AlFh | grep -E name\" | :calc { /* | */ return true || false; }\n:calc { return \"|\"; }";
    let source = SourceText::new("pipe", text);
    let tokens = lex(&source);
    assert_eq!(
        tokens
            .tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Pipe)
            .count(),
        1
    );
    let parsed = parse(&source);
    assert!(parsed.diagnostics.is_empty());
    assert_eq!(parsed.script.statements.len(), 2);
    assert!(matches!(
        parsed.script.statements[1].expression,
        Expression::Calculation(_)
    ));
}

#[test]
fn malformed_or_oversized_pipelines_are_rejected() {
    for text in [
        "| first read",
        "first read |",
        "first read || second write",
        "first read |\n",
        "first read > | second write",
    ] {
        assert!(
            !parse(&SourceText::new("bad", text)).diagnostics.is_empty(),
            "{text}"
        );
    }
    let text = vec![":calc { return 1; }"; 1001].join(" | ");
    assert!(
        parse(&SourceText::new("large", text))
            .diagnostics
            .iter()
            .any(|d| d.code == "PIP001")
    );
}

#[test]
fn fork_blocks_selectors_and_nested_pipeline_roundtrip_with_bounds() {
    let text = include_str!("../../../examples/pipeline-forks/finite.wes");
    let parsed = parse(&SourceText::new("fork", text));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let printed = parsed.script.to_string();
    let again = parse(&SourceText::new("roundtrip", &printed));
    assert!(again.diagnostics.is_empty(), "{:?}", again.diagnostics);
    assert_eq!(again.script.to_string(), printed);
    for source in [
        "$a | :fork {}",
        "$a | :fork { on error {} }",
        "$a | :fork { { foo }",
        "$a | :fork { when {} }",
    ] {
        assert!(
            !parse(&SourceText::new("bad", source))
                .diagnostics
                .is_empty(),
            "{source}"
        );
    }
    let deep = format!("$a | {}foo{}", ":fork { { ".repeat(33), " } }".repeat(33));
    assert!(
        parse(&SourceText::new("deep", &deep))
            .diagnostics
            .iter()
            .any(|d| d.code == "PIP004")
    );
}
