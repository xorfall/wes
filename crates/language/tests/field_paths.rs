use wes_language::{Expression, SourceText, TokenKind, Value, lex, parse};

#[test]
fn record_paths_are_single_references_with_exact_spans_and_preserved_output_selector() {
    let text = "catalog use id:$ürün.address.city code:$job::error.code literal:\"$item.id\"";
    let source = SourceText::new("field-paths", text);
    let lexed = lex(&source);
    assert!(lexed.diagnostics.is_empty(), "{:?}", lexed.diagnostics);
    let refs: Vec<_> = lexed
        .tokens
        .iter()
        .filter(|t| t.kind == TokenKind::Ref)
        .collect();
    assert_eq!(
        refs.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
        ["ürün.address.city", "job::error.code"]
    );
    for token in refs {
        assert_eq!(
            &text[token.span.start()..token.span.end()],
            format!("${}", token.text)
        );
    }
    let parsed = parse(&source);
    assert!(parsed.diagnostics.is_empty());
    let Expression::Call(call) = &parsed.script.statements[0].expression else {
        panic!("call");
    };
    assert!(matches!(call.arguments[0].value, Value::Reference(_)));
    assert!(matches!(call.arguments[2].value, Value::Text(_)));
}

#[test]
fn empty_field_segments_are_refused() {
    for reference in ["$item.", "$item..id", "$item.id.", "$item::error."] {
        let parsed = parse(&SourceText::new(
            "bad-path",
            format!("catalog use id:{reference}"),
        ));
        assert!(
            parsed.diagnostics.iter().any(|d| d.code == "LEX002"),
            "{reference}"
        );
    }
}
