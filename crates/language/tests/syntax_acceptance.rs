use serde_json::{Value, json};
use wes_language::{SourceText, lex, parse};

fn fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/syntax-acceptance.json"
    ))
    .unwrap()
}

#[test]
fn tokens_and_utf16_spans_match_acceptance_fixtures() {
    for fixture in fixtures() {
        let input = fixture["input"].as_str().unwrap();
        let source = SourceText::new("fixture", input);
        let result = lex(&source);
        let tokens: Vec<_> = result
            .tokens
            .iter()
            .map(|t| {
                json!({
                    "kind":t.kind.fixture_name(), "text":t.text,
                    "start":source.utf16_offset(t.span.start()).unwrap(),
                    "end":source.utf16_offset(t.span.end()).unwrap(),
                })
            })
            .collect();
        assert_eq!(json!(tokens), fixture["tokens"], "input: {input:?}");
    }
}

#[test]
fn parser_diagnostics_and_printing_match_acceptance_fixtures() {
    for fixture in fixtures() {
        let input = fixture["input"].as_str().unwrap();
        let source = SourceText::new("fixture", input);
        let result = parse(&source);
        let diagnostics: Vec<_> = result
            .diagnostics
            .iter()
            .map(|d| {
                json!({
                    "code":d.code, "start":source.utf16_offset(d.span.start()).unwrap(),
                    "end":source.utf16_offset(d.span.end()).unwrap(),
                })
            })
            .collect();
        assert_eq!(
            json!(diagnostics),
            fixture["diagnostics"],
            "input: {input:?}"
        );
        assert_eq!(
            result.script.to_string(),
            fixture["printed"].as_str().unwrap(),
            "input: {input:?}"
        );
    }
}

#[test]
fn valid_acceptance_cases_round_trip_without_new_errors() {
    for fixture in fixtures() {
        if !fixture["diagnostics"].as_array().unwrap().is_empty() {
            continue;
        }
        let printed = fixture["printed"].as_str().unwrap();
        let result = parse(&SourceText::new("roundtrip", printed));
        assert!(
            result.diagnostics.is_empty(),
            "input: {printed:?}: {:?}",
            result.diagnostics
        );
        assert_eq!(result.script.to_string(), printed);
    }
}
