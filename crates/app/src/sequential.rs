//! Explicit runner sequencing, using the language parser rather than a second grammar.
use wes_engine::source::SourceInput;

pub(super) fn split(input: &SourceInput) -> Result<Vec<wes_language::Span>, super::Error> {
    let source = wes_language::SourceText::new(input.source_name(), input.text());
    input.statement_spans().map_err(|diagnostics| {
        let error = diagnostics
            .iter()
            .find(|d| d.severity == wes_language::Severity::Error)
            .expect("syntax error");
        let position = source.position(error.span.start()).expect("parser span");
        std::io::Error::other(format!(
            "{}:{}:{}: {error}; no workflow step was executed",
            input.source_name(),
            position.line,
            position.column
        ))
        .into()
    })
}
pub(super) fn statement(input: &SourceInput, span: wes_language::Span) -> &str {
    &input.text()[span.start()..span.end()]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uses_top_level_spans_and_preserves_unicode_offsets() {
        let text = ":calc { return 'é'; } > first\n// comment\n:calc { return $first; } > second";
        let steps = split(&SourceInput::new("cell".into(), text.into()).unwrap()).unwrap();
        assert_eq!(steps.len(), 2);
        let input = SourceInput::new("cell".into(), text.into()).unwrap();
        let step = statement(&input, steps[1]);
        assert_eq!(step, ":calc { return $first; } > second");
        assert_eq!(step.matches('\n').count(), 0);
        assert!(!step.contains("é"));
    }
    #[test]
    fn syntax_is_checked_before_any_step() {
        assert!(
            split(
                &SourceInput::new("cell".into(), ":workspace save \"copy\"\n:calc {".into())
                    .unwrap()
            )
            .is_err()
        );
    }
}
