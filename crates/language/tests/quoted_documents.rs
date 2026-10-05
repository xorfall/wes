use wes_language::{Expression, SourceText, Value, parse, quote_text};
#[test]
fn quoted_documents_preserve_line_endings_tabs_unicode_quotes_and_literal_backslashes() {
    let original = "# ĞğİıŞşÇçÖöÜü\r\n\tkey: \"quoted\"\nblank:\n\n  path: C:\\new\\test\n";
    let source = format!(":package load source:{}", quote_text(original));
    let parsed = parse(&SourceText::new("fixture", &source));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let Expression::Call(call) = &parsed.script.statements[0].expression else {
        panic!("call")
    };
    let Value::Text(value) = &call.arguments[0].value else {
        panic!("text")
    };
    assert_eq!(value.text, original);
    let printed = parsed.script.statements[0].to_string();
    let again = parse(&SourceText::new("roundtrip", &printed));
    assert!(again.diagnostics.is_empty());
    let Expression::Call(call) = &again.script.statements[0].expression else {
        panic!("call")
    };
    assert_eq!(call.arguments[0].value.to_string(), quote_text(original));
}
