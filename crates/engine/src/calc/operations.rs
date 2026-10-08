use super::{Failure, Machine, value::Item};
use crate::driver::CancellationToken;
use std::{cmp::Ordering, sync::Arc};
use wes_core::{Data, Decimal, DecimalOp, NumericError};
use wes_language::{
    Span,
    calc::{Binary, Operation, Unary},
};

fn number(error: NumericError, span: Span) -> Failure {
    Failure::new(
        if error == NumericError::Limit {
            "CAL006"
        } else {
            "CAL005"
        },
        span,
        error.to_string(),
    )
}
fn overflow(span: Span) -> Failure {
    Failure::new(
        "CAL005",
        span,
        "integer arithmetic overflow; result must fit a signed 64-bit Int",
    )
}
fn index_bounds(kind: &str, index: i64, length: usize, span: Span) -> Failure {
    let bounds = if length == 0 {
        format!("{kind} is empty; no valid index")
    } else {
        format!(
            "{kind} length is {length}; valid indices are 0..{} (inclusive)",
            length - 1
        )
    };
    Failure::new(
        wes_language::calc::diagnostics::Category::Bounds.code(),
        span,
        format!("index {index} is out of range; {bounds}"),
    )
}
impl Machine {
    pub(super) fn compare_order(
        &mut self,
        left: &Item,
        right: &Item,
        span: Span,
        operator: &str,
    ) -> Result<Ordering, Failure> {
        let (Item::Scalar(a), Item::Scalar(b)) = (left.untyped(), right.untyped()) else {
            return Err(Failure::new(
                "CAL004",
                span,
                format!(
                    "{operator} requires matching Int, Decimal, Text, Instant or Duration; received {} and {}; filter missing keys explicitly",
                    left.kind(),
                    right.kind()
                ),
            ));
        };
        Ok(match (a.as_ref(), b.as_ref()) {
            (Data::Int(a), Data::Int(b)) => a.cmp(b),
            (Data::Instant(a), Data::Instant(b)) => a.cmp(b),
            (Data::Duration(a), Data::Duration(b)) => a.cmp(b),
            (Data::Decimal(a), Data::Decimal(b)) => {
                self.numeric_cost(a, b, span)?;
                a.compare_numeric(b, 1024).map_err(|e| number(e, span))?
            }
            (Data::Text(a), Data::Text(b)) => {
                self.budget.work((a.len() + b.len()) as u64, span)?;
                a.cmp(b)
            }
            _ => {
                return Err(Failure::new(
                    "CAL004",
                    span,
                    format!(
                        "comparison '{}' requires matching Int, Decimal, Text, Instant or Duration; received {} and {}",
                        operator,
                        left.kind(),
                        right.kind()
                    ),
                ));
            }
        })
    }
    fn numeric_cost(&mut self, a: &Decimal, b: &Decimal, span: Span) -> Result<(), Failure> {
        let cost = a
            .compact_text_size_bound()
            .saturating_add(b.compact_text_size_bound())
            .saturating_add((a.scale() - b.scale()).unsigned_abs())
            .max(1);
        self.budget.work(cost.saturating_mul(cost), span)?;
        self.budget.allocate(cost.saturating_mul(8), span)
    }
    pub(super) fn unary(&mut self, op: Unary, value: Item, span: Span) -> Result<Item, Failure> {
        if op == Unary::Not {
            return Ok(Item::scalar(Data::Bool(!value.bool(span)?)));
        }
        let Item::Scalar(data) = value.untyped() else {
            return Err(Failure::new(
                "CAL004",
                span,
                format!(
                    "unary numeric operation requires Int or Decimal; received {}",
                    value.kind()
                ),
            ));
        };
        Ok(Item::scalar(match (op, data.as_ref()) {
            (Unary::Positive, Data::Int(_) | Data::Decimal(_)) => data.as_ref().clone(),
            (Unary::Negate, Data::Int(n)) => {
                Data::Int(n.checked_neg().ok_or_else(|| overflow(span))?)
            }
            (Unary::Negate, Data::Decimal(n)) => {
                let zero: Decimal = "0".parse().expect("zero");
                self.numeric_cost(&zero, n, span)?;
                Data::Decimal(
                    zero.calculate(DecimalOp::Subtract, n, 1024)
                        .map_err(|e| number(e, span))?,
                )
            }
            _ => {
                return Err(Failure::new(
                    "CAL004",
                    span,
                    format!(
                        "unary numeric operation requires Int or Decimal; received {}",
                        value.kind()
                    ),
                ));
            }
        }))
    }
    pub(super) fn binary(
        &mut self,
        op: Binary,
        left: Item,
        right: Item,
        span: Span,
    ) -> Result<Item, Failure> {
        if matches!(op, Binary::Eq | Binary::Ne) {
            let eq = self.equal(&left, &right, span)?;
            return Ok(Item::scalar(Data::Bool(if op == Binary::Eq {
                eq
            } else {
                !eq
            })));
        }
        let (Item::Scalar(a), Item::Scalar(b)) = (left.untyped(), right.untyped()) else {
            return Err(Failure::new(
                "CAL004",
                span,
                format!(
                    "operation '{}' cannot combine {} and {}",
                    op.id(),
                    left.kind(),
                    right.kind()
                ),
            ));
        };
        if matches!(op, Binary::Lt | Binary::Le | Binary::Gt | Binary::Ge) {
            let order = self.compare_order(&left, &right, span, op.id())?;
            return Ok(Item::scalar(Data::Bool(match op {
                Binary::Lt => order == Ordering::Less,
                Binary::Le => order != Ordering::Greater,
                Binary::Gt => order == Ordering::Greater,
                _ => order != Ordering::Less,
            })));
        }
        let result = match (a.as_ref(), b.as_ref()) {
            (Data::Instant(a), Data::Duration(b)) if matches!(op, Binary::Add | Binary::Sub) => {
                Data::Instant(
                    (if op == Binary::Add {
                        a.add(*b)
                    } else {
                        a.subtract(*b)
                    })
                    .map_err(|e| super::temporal::temporal_error(e, span))?,
                )
            }
            (Data::Duration(a), Data::Instant(b)) if op == Binary::Add => Data::Instant(
                b.add(*a)
                    .map_err(|e| super::temporal::temporal_error(e, span))?,
            ),
            (Data::Instant(a), Data::Instant(b)) if op == Binary::Sub => Data::Duration(
                a.since(*b)
                    .map_err(|e| super::temporal::temporal_error(e, span))?,
            ),
            (Data::Duration(a), Data::Duration(b)) if matches!(op, Binary::Add | Binary::Sub) => {
                Data::Duration(
                    (if op == Binary::Add {
                        a.add(*b)
                    } else {
                        a.subtract(*b)
                    })
                    .map_err(|e| super::temporal::temporal_error(e, span))?,
                )
            }
            (Data::Duration(a), Data::Duration(b)) if op == Binary::Divide => {
                let a: Decimal = a.total_nanos().to_string().parse().expect("duration nanos");
                let b: Decimal = b.total_nanos().to_string().parse().expect("duration nanos");
                self.decimal_with_hint(DecimalOp::Divide, &a, &b, span, Some("division is nonterminating; use roundDiv(toNanos(a), toNanos(b), scale) for an explicitly rounded Duration ratio"))?
            }
            (Data::Duration(d), Data::Int(_) | Data::Decimal(_))
                if matches!(op, Binary::Mul | Binary::Divide) =>
            {
                self.scale_duration(*d, b.as_ref(), op, span)?
            }
            (Data::Int(_) | Data::Decimal(_), Data::Duration(d)) if op == Binary::Mul => {
                self.scale_duration(*d, a.as_ref(), op, span)?
            }
            (Data::Text(a), Data::Text(b)) if op == Binary::Add => {
                let length = a
                    .len()
                    .checked_add(b.len())
                    .ok_or_else(|| Failure::new("CAL006", span, "text limit"))?;
                self.budget.work(length as u64, span)?;
                self.budget.allocate(length as u64, span)?;
                Data::Text(format!("{a}{b}").into())
            }
            (Data::Int(a), Data::Int(b)) if op != Binary::Divide => Data::Int(
                match op {
                    Binary::Add => a.checked_add(*b),
                    Binary::Sub => a.checked_sub(*b),
                    Binary::Mul => a.checked_mul(*b),
                    _ => None,
                }
                .ok_or_else(|| overflow(span))?,
            ),
            (Data::Int(a), Data::Int(b)) => {
                let a = a.to_string().parse().expect("int decimal");
                let b = b.to_string().parse().expect("int decimal");
                self.decimal(DecimalOp::Divide, &a, &b, span)?
            }
            (Data::Decimal(a), Data::Decimal(b)) => self.decimal(
                match op {
                    Binary::Add => DecimalOp::Add,
                    Binary::Sub => DecimalOp::Subtract,
                    Binary::Mul => DecimalOp::Multiply,
                    Binary::Divide => DecimalOp::Divide,
                    _ => return Err(Failure::new("CAL004", span, "invalid decimal operation")),
                },
                a,
                b,
                span,
            )?,
            _ => {
                return Err(Failure::new(
                    "CAL004",
                    span,
                    format!(
                        "operation '{}' requires matching kinds and explicit conversions; received {} and {}",
                        op.id(),
                        left.kind(),
                        right.kind()
                    ),
                ));
            }
        };
        Ok(Item::scalar(result))
    }
    fn scale_duration(
        &mut self,
        duration: wes_core::DurationValue,
        scalar: &Data,
        op: Binary,
        span: Span,
    ) -> Result<Data, Failure> {
        let a: Decimal = duration
            .total_nanos()
            .to_string()
            .parse()
            .expect("duration nanos");
        let b = match scalar {
            Data::Decimal(n) => n.clone(),
            Data::Int(n) => n.to_string().parse().expect("int decimal"),
            _ => unreachable!(),
        };
        let Data::Decimal(result) = self.decimal_with_hint(
            if op == Binary::Mul {
                DecimalOp::Multiply
            } else {
                DecimalOp::Divide
            },
            &a,
            &b,
            span,
            Some("duration division requires exact nanoseconds; for explicit rounding use durationNanos(roundDiv(toNanos(total), count, 0))"),
        )?
        else {
            unreachable!()
        };
        let nanos = result.exact_scaled_i128(0).ok_or_else(|| Failure::new("CAL005", span, "duration scaling requires exact whole nanoseconds within the supported range; for explicit division rounding use durationNanos(roundDiv(toNanos(total), count, 0))"))?;
        Ok(Data::Duration(
            wes_core::DurationValue::from_nanos(nanos)
                .map_err(|e| super::temporal::temporal_error(e, span))?,
        ))
    }
    fn decimal(
        &mut self,
        op: DecimalOp,
        a: &Decimal,
        b: &Decimal,
        span: Span,
    ) -> Result<Data, Failure> {
        self.decimal_with_hint(op, a, b, span, None)
    }
    fn decimal_with_hint(
        &mut self,
        op: DecimalOp,
        a: &Decimal,
        b: &Decimal,
        span: Span,
        nonterminating_hint: Option<&str>,
    ) -> Result<Data, Failure> {
        self.numeric_cost(a, b, span)?;
        Ok(Data::Decimal(a.calculate(op, b, 1024).map_err(|e| {
            if e == NumericError::Nonterminating
                && let Some(hint) = nonterminating_hint
            {
                Failure::new("CAL005", span, hint)
            } else {
                number(e, span)
            }
        })?))
    }
    fn equal(&mut self, a: &Item, b: &Item, span: Span) -> Result<bool, Failure> {
        let mut pending = vec![(a, b)];
        while let Some((a, b)) = pending.pop() {
            self.budget.work(1, span)?;
            match (a.untyped(), b.untyped()) {
                (Item::Scalar(a), Item::Scalar(b)) => {
                    if let (Data::Decimal(a), Data::Decimal(b)) = (a.as_ref(), b.as_ref()) {
                        self.numeric_cost(a, b, span)?;
                        if a.compare_numeric(b, 1024).map_err(|e| number(e, span))?
                            != Ordering::Equal
                        {
                            return Ok(false);
                        }
                    } else {
                        let size = match (a.as_ref(), b.as_ref()) {
                            (Data::Text(a), Data::Text(b)) => a.len() + b.len(),
                            (Data::Bytes(a), Data::Bytes(b)) => a.len() + b.len(),
                            _ => 1,
                        };
                        self.budget.work(size as u64, span)?;
                        if a != b {
                            return Ok(false);
                        }
                    }
                }
                (Item::Option(None), Item::Option(None)) => {}
                (Item::Option(Some(a)), Item::Option(Some(b))) => pending.push((a, b)),
                (Item::List(a), Item::List(b)) => {
                    if a.len() != b.len() {
                        return Ok(false);
                    }
                    self.budget.work(a.len() as u64, span)?;
                    pending.extend(a.iter().zip(b.iter()));
                }
                (Item::Record(a), Item::Record(b)) => {
                    if a.len() != b.len() {
                        return Ok(false);
                    }
                    self.budget.work(a.len() as u64, span)?;
                    for (key, value) in a.iter() {
                        let Some(other) = b.get(key) else {
                            return Ok(false);
                        };
                        pending.push((value, other));
                    }
                }
                (
                    Item::Iter(_)
                    | Item::IterNamespace
                    | Item::Function { .. }
                    | Item::Builtin(_)
                    | Item::Method(..),
                    _,
                )
                | (
                    _,
                    Item::Iter(_)
                    | Item::IterNamespace
                    | Item::Function { .. }
                    | Item::Builtin(_)
                    | Item::Method(..),
                ) => {
                    return Err(Failure::new(
                        "CAL004",
                        span,
                        "functions have no data equality",
                    ));
                }
                _ => return Ok(false),
            }
        }
        Ok(true)
    }
    pub(super) fn index(&mut self, item: Item, index: Item, span: Span) -> Result<Item, Failure> {
        match item.untyped() {
            Item::Iter(_) => Err(Failure::new("CAL004",span,"Iter does not support indexing; use .take(1).collect() for a bounded list, then index that list")),
            Item::List(items) => {
                let requested = index.int(span)?;
                let at = usize::try_from(requested).map_err(|_| {
                    index_bounds("List", requested, items.len(), span)
                })?;
                items.get(at).cloned().map(|v| {
                    let v = if let Item::Typed(_, shape, _) = &item && let wes_core::Shape::List(element) = shape.as_ref() { v.typed(element.as_ref().clone()) } else { v };
                    item.project_annotation(v, "/e")
                }).ok_or_else(|| index_bounds("List", requested, items.len(), span))
            }
            Item::Record(fields) => fields
                .get(index.text(span)?)
                .cloned()
                .map(|v| item.project_field_shape(v, index.text(span).expect("checked text")))
                .ok_or_else(|| Failure::new("CAL004", span,
                    "requested record field is absent; use has(record, key) to check presence or keys(record) to inspect field names")),
            Item::Scalar(data) => {
                if let Data::Text(text) = data.as_ref() {
                    let requested = index.int(span)?;
                    let at = usize::try_from(requested).map_err(|_| {
                        Failure::categorized(wes_language::calc::diagnostics::Category::Bounds, span, format!("Text index {requested} is negative; indices start at 0"))
                    })?;
                    self.budget.work(text.len() as u64, span)?;
                    let mut chars = text.chars();
                    let skipped = chars.by_ref().take(at).count();
                    let ch = chars.next().ok_or_else(|| index_bounds("Text", requested, skipped, span))?;
                    Ok(Item::scalar(Data::Text(ch.to_string().into())))
                } else {
                    Err(item.expected("List, Record or Text for indexing", span))
                }
            }
            _ => Err(item.expected("List, Record or Text for indexing", span)),
        }
    }
    pub(super) fn builtin(
        &mut self,
        operation: Operation,
        args: Vec<Item>,
        span: Span,
        token: &CancellationToken,
    ) -> Result<Item, Failure> {
        let value = match operation {
            Operation::RegexTest | Operation::StripAnsi => {
                return self.text_builtin(operation, &args, span);
            }
            Operation::Instant
            | Operation::Duration
            | Operation::Interval
            | Operation::Around
            | Operation::FromEpochSeconds
            | Operation::FromEpochMillis
            | Operation::FromEpochNanos
            | Operation::ToEpochSeconds
            | Operation::ToEpochMillis
            | Operation::ToEpochNanos
            | Operation::DurationSeconds
            | Operation::DurationMillis
            | Operation::DurationNanos
            | Operation::ToSeconds
            | Operation::ToMillis
            | Operation::ToNanos
            | Operation::UtcParts => return self.temporal_builtin(operation, &args, span),
            Operation::Some => Item::Option(Some(Arc::new(args[0].clone()))),
            Operation::IsSome => {
                if let Item::Option(v) = args[0].untyped() {
                    Item::scalar(Data::Bool(v.is_some()))
                } else {
                    return Err(Failure::new(
                        "CAL004",
                        span,
                        format!("isSome requires Option; received {}", args[0].kind()),
                    ));
                }
            }
            Operation::UnwrapOr => {
                if let Item::Option(v) = args[0].untyped() {
                    if let Some(v) = v {
                        let v = if let Item::Typed(_, shape, _) = &args[0]
                            && let wes_core::Shape::Option(inner) = shape.as_ref()
                        {
                            v.as_ref().clone().typed(inner.as_ref().clone())
                        } else {
                            v.as_ref().clone()
                        };
                        args[0].project_annotation(v, "/o")
                    } else {
                        args[1].clone()
                    }
                } else {
                    return Err(Failure::new(
                        "CAL004",
                        span,
                        format!("unwrapOr requires Option; received {}", args[0].kind()),
                    ));
                }
            }
            Operation::Has => {
                if let Item::Record(fields) = args[0].untyped() {
                    Item::scalar(Data::Bool(fields.contains_key(args[1].text(span)?)))
                } else {
                    return Err(Failure::new(
                        "CAL004",
                        span,
                        format!("has requires a record; received {}", args[0].kind()),
                    ));
                }
            }
            Operation::Length => {
                let len = match args[0].untyped() {
                    Item::Iter(_) => {
                        return Err(Failure::new(
                            "CAL004",
                            span,
                            "length does not accept Iter; use .count() to traverse and count it, or .take(n).collect() for a bounded list",
                        ));
                    }
                    Item::List(v) => v.len(),
                    Item::Record(v) => v.len(),
                    Item::Scalar(v) => match v.as_ref() {
                        Data::Text(s) => {
                            self.budget.work(s.len() as u64, span)?;
                            s.chars().count()
                        }
                        Data::Bytes(b) => b.len(),
                        _ => {
                            return Err(Failure::new(
                                "CAL004",
                                span,
                                format!(
                                    "length requires List, Record, Text or Bytes; received {}",
                                    args[0].kind()
                                ),
                            ));
                        }
                    },
                    _ => {
                        return Err(Failure::new(
                            "CAL004",
                            span,
                            format!(
                                "length requires List, Record, Text or Bytes; received {}",
                                args[0].kind()
                            ),
                        ));
                    }
                };
                Item::scalar(Data::Int(i64::try_from(len).map_err(|_| overflow(span))?))
            }
            Operation::Keys => {
                let Item::Record(fields) = args[0].untyped() else {
                    return Err(Failure::new(
                        "CAL004",
                        span,
                        format!("keys requires a record; received {}", args[0].kind()),
                    ));
                };
                let mut items = vec![];
                for key in fields.keys() {
                    self.budget.work(1, span)?;
                    self.budget.allocate(key.len() as u64 + 96, span)?;
                    items.push(Item::scalar(Data::Text(key.as_str().into())));
                }
                Item::List(Arc::new(items))
            }
            Operation::Range => {
                let (start, end) = if args.len() == 1 {
                    (0, args[0].int(span)?)
                } else {
                    (args[0].int(span)?, args[1].int(span)?)
                };
                let step = if args.len() == 3 {
                    args[2].int(span)?
                } else {
                    1
                };
                if step == 0 {
                    return Err(Failure::new("CAL005", span, "range step must be nonzero"));
                }
                let distance = if step > 0 {
                    i128::from(end) - i128::from(start)
                } else {
                    i128::from(start) - i128::from(end)
                };
                let step_size = i128::from(step).abs();
                let count = if distance <= 0 {
                    0
                } else {
                    (distance + step_size - 1) / step_size
                };
                let count = u64::try_from(count)
                    .map_err(|_| Failure::new("CAL006", span, "range size exceeds limit"))?;
                self.budget.work(count, span)?;
                self.budget.allocate(count.saturating_mul(96), span)?;
                let mut items = Vec::new();
                for i in 0..count {
                    if token.is_cancelled() {
                        return Err(Failure::cancelled(span));
                    }
                    let n = i128::from(start) + i128::from(i) * i128::from(step);
                    items.push(Item::scalar(Data::Int(
                        i64::try_from(n).map_err(|_| overflow(span))?,
                    )));
                }
                Item::List(Arc::new(items))
            }
            Operation::Decimal => {
                let text = match args[0].untyped() {
                    Item::Scalar(v) => match v.as_ref() {
                        Data::Decimal(_) => return Ok(args[0].clone()),
                        Data::Int(n) => std::borrow::Cow::Owned(n.to_string()),
                        Data::Text(s) => std::borrow::Cow::Borrowed(s.as_ref()),
                        _ => {
                            return Err(Failure::new(
                                "CAL004",
                                span,
                                format!(
                                    "decimal requires Int, Decimal or numeric Text; received {}",
                                    args[0].kind()
                                ),
                            ));
                        }
                    },
                    _ => {
                        return Err(Failure::new(
                            "CAL004",
                            span,
                            format!(
                                "decimal requires Int, Decimal or numeric Text; received {}",
                                args[0].kind()
                            ),
                        ));
                    }
                };
                self.budget.work(text.len() as u64, span)?;
                self.budget.allocate(text.len() as u64 * 2, span)?;
                Item::scalar(Data::Decimal(text.parse().map_err(|_| {
                    Failure::new(
                        wes_language::calc::diagnostics::Category::Parse.code(),
                        span,
                        "decimal cannot parse Text as a decimal number; use numeric Text with a decimal point or exponent",
                    )
                })?))
            }
            Operation::Int => {
                let n = match args[0].untyped() {
                    Item::Scalar(v) => match v.as_ref() {
                        Data::Int(n) => Some(*n),
                        Data::Decimal(n) => {
                            self.budget.work(n.compact_text_size_bound(), span)?;
                            n.exact_i64()
                        }
                        Data::Text(s) => {
                            self.budget.work(s.len() as u64, span)?;
                            Some(s.parse::<i64>().map_err(|error| {
                                let code = match error.kind() {
                                    std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow => "CAL005",
                                    _ => wes_language::calc::diagnostics::Category::Parse.code(),
                                };
                                Failure::new(code, span, "int cannot parse Text as an exact signed 64-bit integer; use integer numeric Text within the Int range")
                            })?)
                        }
                        _ => return Err(args[0].expected("Int, Decimal or numeric Text", span)),
                    },
                    _ => return Err(args[0].expected("Int, Decimal or numeric Text", span)),
                }
                .ok_or_else(|| {
                    Failure::new(
                        "CAL005",
                        span,
                        format!(
                            "int cannot convert {} to an exact signed 64-bit integer",
                            args[0].kind()
                        ),
                    )
                })?;
                Item::scalar(Data::Int(n))
            }
            Operation::Text => {
                let Item::Scalar(v) = args[0].untyped() else {
                    return Err(Failure::new(
                        "CAL004",
                        span,
                        format!("text requires a scalar; received {}", args[0].kind()),
                    ));
                };
                let text = match v.as_ref() {
                    Data::Bytes(bytes) => {
                        // Explicit, strict decoding: validate bounded work and reserve the output
                        // before scanning/copying. Never replace invalid bytes or echo the input.
                        self.budget.work(bytes.len() as u64, span)?;
                        self.budget.allocate(bytes.len() as u64, span)?;
                        let text = std::str::from_utf8(bytes).map_err(|error| {
                            Failure::new(
                                wes_language::calc::diagnostics::Category::Parse.code(),
                                span,
                                format!(
                                    "text requires valid UTF-8 Bytes; invalid sequence at byte {}",
                                    error.valid_up_to()
                                ),
                            )
                        })?;
                        return Ok(Item::scalar(Data::Text(text.into())));
                    }
                    Data::Text(s) => s.clone(),
                    Data::Int(n) => n.to_string().into(),
                    Data::Decimal(n) => {
                        self.budget.work(n.compact_text_size_bound(), span)?;
                        n.to_string().into()
                    }
                    Data::Bool(b) => b.to_string().into(),
                    Data::Instant(t) => t.to_string().into(),
                    Data::Duration(t) => t.to_string().into(),
                    Data::Interval(t) => t.to_string().into(),
                    _ => {
                        return Err(Failure::new(
                            "CAL004",
                            span,
                            format!(
                                "text requires a printable scalar; received {}",
                                args[0].kind()
                            ),
                        ));
                    }
                };
                self.budget.allocate(text.len() as u64, span)?;
                Item::scalar(Data::Text(text))
            }
            Operation::Div | Operation::Rem => {
                let a = args[0].int(span)?;
                let b = args[1].int(span)?;
                if b == 0 {
                    return Err(Failure::new(
                        "CAL005",
                        span,
                        format!("{} divisor must not be zero", operation.id()),
                    ));
                }
                Item::scalar(Data::Int(
                    if operation == Operation::Div {
                        a.checked_div(b)
                    } else {
                        a.checked_rem(b)
                    }
                    .ok_or_else(|| overflow(span))?,
                ))
            }
            Operation::RoundDiv => {
                let convert = |v: &Item| -> Result<Decimal, Failure> {
                    match v.untyped() {
                        Item::Scalar(n) => match n.as_ref() {
                            Data::Int(n) => Ok(n.to_string().parse().expect("int")),
                            Data::Decimal(n) => Ok(n.clone()),
                            _ => Err(Failure::new(
                                "CAL004",
                                span,
                                format!(
                                    "roundDiv requires Int or Decimal operands; received {}",
                                    v.kind()
                                ),
                            )),
                        },
                        _ => Err(Failure::new(
                            "CAL004",
                            span,
                            format!(
                                "roundDiv requires Int or Decimal operands; received {}",
                                v.kind()
                            ),
                        )),
                    }
                };
                let a = convert(&args[0])?;
                let b = convert(&args[1])?;
                let scale = u32::try_from(args[2].int(span)?)
                    .map_err(|_| Failure::new("CAL005", span, "roundDiv scale must be 0..1024"))?;
                if scale > 1024 {
                    return Err(Failure::new("CAL006", span, "roundDiv scale exceeds 1024"));
                }
                self.budget
                    .work(u64::from(scale).saturating_mul(u64::from(scale)), span)?;
                Item::scalar(self.decimal(DecimalOp::RoundDivide(scale), &a, &b, span)?)
            }
            _ => {
                return Err(Failure::new(
                    "CAL003",
                    span,
                    "operation requires a continuation",
                ));
            }
        };
        Ok(value)
    }
}
