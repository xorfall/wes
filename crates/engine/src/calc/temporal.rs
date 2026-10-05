//! Exact temporal builtins. No clock reads or implicit epoch units.
use super::{Failure, Machine, value::Item};
use wes_core::{Data, Decimal, DurationValue, Interval, ModelError, Timestamp};
use wes_language::{Span, calc::Operation};

pub(super) fn temporal_error(error: ModelError, span: Span) -> Failure {
    Failure::new("CAL005", span, error.to_string())
}
impl Machine {
    pub(super) fn temporal_builtin(
        &mut self,
        op: Operation,
        args: &[Item],
        span: Span,
    ) -> Result<Item, Failure> {
        let scalar = |i: usize| match args[i].untyped() {
            Item::Scalar(data) => Ok(data.as_ref()),
            _ => Err(args[i].expected("temporal scalar", span)),
        };
        let instant = |i: usize| match scalar(i)? {
            Data::Instant(t) => Ok(*t),
            _ => Err(args[i].expected("Instant", span)),
        };
        let data = match op {
            Operation::Instant | Operation::Duration => {
                let Data::Text(text) = scalar(0)? else {
                    return Err(args[0].expected("ISO Text", span));
                };
                self.budget.work(text.len() as u64, span)?;
                if text.len() > 256 {
                    return Err(Failure::new(
                        "CAL006",
                        span,
                        "temporal text exceeds 256 bytes",
                    ));
                }
                if op == Operation::Instant {
                    Data::Instant(text.parse().map_err(|e: ModelError| {
                        Failure::categorized(
                            wes_language::calc::diagnostics::Category::Parse,
                            span,
                            e.to_string(),
                        )
                    })?)
                } else {
                    Data::Duration(text.parse().map_err(|e: ModelError| {
                        Failure::categorized(
                            wes_language::calc::diagnostics::Category::Parse,
                            span,
                            e.to_string(),
                        )
                    })?)
                }
            }
            Operation::Interval => Data::Interval(
                Interval::new(instant(0)?, instant(1)?).map_err(|e| temporal_error(e, span))?,
            ),
            Operation::Around => {
                let Data::Duration(radius) = scalar(1)? else {
                    return Err(args[1].expected("Duration", span));
                };
                Data::Interval(
                    Interval::around(instant(0)?, *radius).map_err(|e| temporal_error(e, span))?,
                )
            }
            Operation::FromEpochSeconds
            | Operation::FromEpochMillis
            | Operation::FromEpochNanos
            | Operation::DurationSeconds
            | Operation::DurationMillis
            | Operation::DurationNanos => {
                let places = match op {
                    Operation::FromEpochSeconds | Operation::DurationSeconds => 9,
                    Operation::FromEpochMillis | Operation::DurationMillis => 6,
                    _ => 0,
                };
                let total = match scalar(0)? {
                    Data::Int(n) => Some(i128::from(*n) * 10i128.pow(places)),
                    Data::Decimal(n) => {
                        let cost = n.compact_text_size_bound();
                        self.budget.work(cost, span)?;
                        self.budget.allocate(cost, span)?;
                        n.exact_scaled_i128(places)
                    }
                    _ => return Err(args[0].expected("Int or Decimal epoch", span)),
                }
                .ok_or_else(|| {
                    Failure::new(
                        "CAL005",
                        span,
                        "numeric time must fit its supported range with exact nanosecond precision",
                    )
                })?;
                if matches!(
                    op,
                    Operation::DurationSeconds
                        | Operation::DurationMillis
                        | Operation::DurationNanos
                ) {
                    Data::Duration(
                        DurationValue::from_nanos(total).map_err(|e| temporal_error(e, span))?,
                    )
                } else {
                    Data::Instant(
                        Timestamp::from_nanos(total).map_err(|e| temporal_error(e, span))?,
                    )
                }
            }
            Operation::ToEpochSeconds
            | Operation::ToEpochMillis
            | Operation::ToEpochNanos
            | Operation::ToSeconds
            | Operation::ToMillis
            | Operation::ToNanos => {
                let scale = match op {
                    Operation::ToEpochSeconds | Operation::ToSeconds => 9,
                    Operation::ToEpochMillis | Operation::ToMillis => 6,
                    _ => 0,
                };
                let n = if matches!(
                    op,
                    Operation::ToSeconds | Operation::ToMillis | Operation::ToNanos
                ) {
                    let Data::Duration(d) = scalar(0)? else {
                        return Err(args[0].expected("Duration", span));
                    };
                    d.total_nanos()
                } else {
                    instant(0)?.total_nanos()
                };
                let value: Decimal = format!("{n}e-{scale}")
                    .parse()
                    .map_err(|e| temporal_error(e, span))?;
                Data::Decimal(value)
            }
            Operation::UtcParts => {
                let values = instant(0)?.utc_parts();
                Data::Record(
                    [
                        "year",
                        "month",
                        "day",
                        "hour",
                        "minute",
                        "second",
                        "nanosecond",
                        "weekday",
                    ]
                    .into_iter()
                    .zip(values)
                    .map(|(key, n)| (key.to_owned(), Data::Int(n)))
                    .collect(),
                )
            }
            _ => unreachable!("only temporal operations routed here"),
        };
        Ok(Item::scalar(data))
    }
}
