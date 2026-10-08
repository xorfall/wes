use super::{Failure, Machine, value::Item};
use wes_core::{
    Data,
    text::{AnsiError, ansi_spans, normalized_shape},
};
use wes_language::{Span, calc::Operation};

impl Machine {
    pub(super) fn admit_regex(&mut self, pattern: &str, span: Span) -> Result<(), Failure> {
        if pattern.len() > 16384 {
            return Err(Failure::new(
                "CAL006",
                span,
                "regex pattern exceeds its 16384-byte limit",
            ));
        }
        self.budget.work(pattern.len() as u64 + 1, span)?;
        if !self.iter_regex.contains(pattern) {
            self.budget.allocate(
                wes_core::IterRegexCache::COMPILED_CHARGE + pattern.len() as u64 * 8,
                span,
            )?;
        }
        Ok(())
    }
    pub(super) fn text_builtin(
        &mut self,
        operation: Operation,
        args: &[Item],
        span: Span,
    ) -> Result<Item, Failure> {
        let source = args[0].text(span)?;
        // Admit full-region work before invoking native scans, including no-match paths.
        self.budget.work(source.len() as u64 + 1, span)?;
        match operation {
            Operation::RegexTest => {
                let pattern = args[1].text(span)?;
                self.admit_regex(pattern, span)?;
                let regex = self
                    .iter_regex
                    .compile(pattern)
                    .map_err(|e| Failure::iteration(e, span))?;
                Ok(Item::scalar(Data::Bool(regex.is_match(source))))
            }
            Operation::StripAnsi => {
                self.budget.allocate(source.len() as u64 + 1024, span)?;
                let mut text = String::with_capacity(source.len());
                let mut spans = Vec::new();
                ansi_spans(source, |part| {
                    // Admit the span object before allocation and copying. No half result escapes.
                    self.budget.work(1, span)?;
                    // Include map capacity, keys, list growth and the typed result's shape.
                    self.budget.allocate(1024, span)?;
                    text.push_str(&source[part.input_start..part.input_end]);
                    spans.push(Data::Record([
                        ("inputStart".into(), Data::Int(part.input_start as i64)),
                        ("inputEnd".into(), Data::Int(part.input_end as i64)),
                        ("outputStart".into(), Data::Int(part.output_start as i64)),
                        ("outputEnd".into(), Data::Int(part.output_end as i64)),
                    ].into()));
                    Ok(())
                }).map_err(|e| match e {
                    AnsiError::Admission(e) => e,
                    AnsiError::Invalid => Failure::new("CAL016", span, "unsupported or incomplete terminal escape; stripAnsi accepts 7-bit CSI and terminated OSC only"),
                })?;
                Ok(Item::scalar(Data::Record(
                    [
                        ("text".into(), Data::Text(text.into())),
                        ("spans".into(), Data::List(spans)),
                    ]
                    .into(),
                ))
                .typed(normalized_shape()))
            }
            _ => unreachable!("text operation dispatch"),
        }
    }
}
