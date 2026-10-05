use wes_language::{Expression, SourceText, TokenKind, lex, parse};

#[test]
fn binding_rules_are_shared_and_provider_names_keep_their_own_grammar() {
    for name in ["error_rate", "ürün_1", "𝒜", "result2", "123"] {
        let parsed = parse(&SourceText::new(
            "fixture",
            format!("some-api get-data > {name}\n:calc {{ return ${name}; }}"),
        ));
        assert!(
            parsed.diagnostics.is_empty(),
            "{name}: {:?}",
            parsed.diagnostics
        );
    }
    for operator in [">", "*>"] {
        for name in ["error-rate", "a.b", "a/b"] {
            let parsed = parse(&SourceText::new(
                "fixture",
                format!("api get {operator} {name}"),
            ));
            assert!(
                parsed.diagnostics.iter().any(|d| d.code == "PAR003"),
                "{name}"
            );
        }
    }
}
#[test]
fn command_comments_preserve_urls_strings_and_source_offsets() {
    let text = "// heading 🦀\r\napi get url:https://example.invalid/x value:\"//literal\" > result // note\r\n// middle\r\n:calc { return $result; }";
    let source = SourceText::new("fixture.wes", text);
    let parsed = parse(&source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.script.statements.len(), 2);
    assert_eq!(
        parsed.script.statements[1].span.start(),
        text.find(":calc").unwrap()
    );
    let Expression::Call(call) = &parsed.script.statements[0].expression else {
        panic!("call")
    };
    assert_eq!(
        call.arguments[0].value.name().text,
        "https://example.invalid/x"
    );
    assert_eq!(call.arguments[1].value.name().text, "//literal");
    assert_eq!(
        lex(&source)
            .tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Newline)
            .count(),
        3
    );
    assert_eq!(
        source
            .position(parsed.script.statements[1].span.start())
            .unwrap()
            .line,
        4
    );
    let only = parse(&SourceText::new("fixture", "// empty script"));
    assert!(only.script.statements.is_empty());
    assert!(only.diagnostics.is_empty());
}

#[test]
fn comments_between_pipeline_stages_do_not_split_the_pipeline() {
    let source = SourceText::new(
        "fixture",
        ":calc { return 1; } // first\n// between\n| // stage\n:calc { return input + 1; } > result // end",
    );
    let parsed = parse(&source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let Expression::Pipeline(stages) = &parsed.script.statements[0].expression else {
        panic!("pipeline")
    };
    assert_eq!(stages.len(), 2);
}
