//! Finite collection continuations. Sorting never monopolizes a poll with a blocking sort.
use super::*;
use std::cmp::Ordering;
use wes_core::{RecordShape, Shape};
use wes_language::calc::merge_collection_shapes;

pub(super) enum FiniteWork {
    Concat(Box<Concat>),
    Select(Box<Sort>),
    Selected(Box<Sort>),
    Merge(Box<Sort>),
    Output(Box<Sort>),
}
pub(super) struct Concat {
    lists: [Arc<Vec<Item>>; 2],
    index: usize,
    output: Vec<Item>,
    element: Shape,
    span: Span,
}
pub(super) struct Sort {
    items: Arc<Vec<Item>>,
    callback: Item,
    output_type: Option<Shape>,
    keys: Vec<Item>,
    order: Vec<usize>,
    scratch: Vec<usize>,
    output: Vec<Item>,
    width: usize,
    base: usize,
    left: usize,
    right: usize,
    middle: usize,
    end: usize,
    span: Span,
}
impl Machine {
    pub(super) fn start_finite(
        &mut self,
        operation: Operation,
        args: Vec<Item>,
        span: Span,
    ) -> Result<(), Failure> {
        let items = args[0].list(span)?;
        if operation == Operation::Concat {
            let declared_element = |item: &Item| match item {
                Item::Typed(_, shape, _) => match shape.as_ref() {
                    Shape::List(element) => element.as_ref().clone(),
                    _ => Shape::Unknown,
                },
                _ => Shape::Unknown,
            };
            let element =
                merge_collection_shapes(&declared_element(&args[0]), &declared_element(&args[1]))
                    .ok_or_else(|| {
                    Failure::new("CAL004", span, "concat requires compatible element types")
                })?;
            self.work
                .push(Work::Finite(FiniteWork::Concat(Box::new(Concat {
                    lists: [items, args[1].list(span)?],
                    index: 0,
                    output: vec![],
                    element,
                    span,
                }))));
        } else {
            let callback = args[1].clone();
            let Item::Function { function, .. } = callback.untyped() else {
                return Err(Failure::new(
                    "CAL004",
                    span,
                    "sortBy requires a verified pure selector function",
                ));
            };
            if !self.compiled.pure_callback(*function) {
                return Err(Failure::new(
                    "CAL009",
                    span,
                    "sortBy selector must be pure and cannot capture mutable outer bindings",
                ));
            }
            let count = items.len();
            self.budget
                .allocate((count as u64).saturating_mul(224), span)?;
            self.work
                .push(Work::Finite(FiniteWork::Select(Box::new(Sort {
                    items,
                    callback,
                    output_type: match &args[0] {
                        Item::Typed(_, shape, _) if matches!(shape.as_ref(), Shape::List(_)) => {
                            Some(shape.as_ref().clone())
                        }
                        _ => None,
                    },
                    keys: Vec::with_capacity(count),
                    order: Vec::with_capacity(count),
                    scratch: Vec::with_capacity(count),
                    output: Vec::with_capacity(count),
                    width: 1,
                    base: 0,
                    left: 0,
                    right: 0,
                    middle: 0,
                    end: 0,
                    span,
                }))));
        }
        Ok(())
    }
    pub(super) fn finite_work(
        &mut self,
        work: FiniteWork,
        token: &CancellationToken,
    ) -> Result<(), Failure> {
        match work {
            FiniteWork::Concat(mut frame) => {
                let first_len = frame.lists[0].len();
                let value = if frame.index < first_len {
                    frame.lists[0].get(frame.index)
                } else {
                    frame.lists[1].get(frame.index - first_len)
                };
                if let Some(value) = value {
                    let shape = self.collection_shape(value, frame.span, 0)?;
                    frame.element = merge_collection_shapes(&frame.element, &shape).ok_or_else(|| Failure::new("CAL004", frame.span,
                        "concat requires compatible element types; map both inputs to a common record or convert numbers explicitly"))?;
                    self.budget.allocate(96, frame.span)?;
                    frame.output.push(value.clone());
                    frame.index += 1;
                    self.work.push(Work::Finite(FiniteWork::Concat(frame)));
                } else {
                    self.values.push(
                        Item::List(Arc::new(frame.output))
                            .typed(Shape::List(Box::new(frame.element))),
                    );
                }
            }
            FiniteWork::Select(frame) => {
                let index = frame.keys.len();
                if index == frame.items.len() {
                    self.work.push(Work::Finite(FiniteWork::Merge(frame)));
                } else {
                    let callback = frame.callback.clone();
                    let value = frame.items[index].clone();
                    let span = frame.span;
                    self.work.push(Work::Finite(FiniteWork::Selected(frame)));
                    if self
                        .invoke(callback, vec![value], usize::MAX, span, token)?
                        .is_some()
                    {
                        return Err(Failure::new(
                            "CAL004",
                            span,
                            "sortBy selector cannot request a provider",
                        ));
                    }
                }
            }
            FiniteWork::Selected(mut frame) => {
                let key = self.pop()?;
                // Even zero comparisons (one record) must reject absent or unsupported keys.
                self.compare_order(
                    frame.keys.first().unwrap_or(&key),
                    &key,
                    frame.span,
                    "sortBy keys",
                )?;
                frame.order.push(frame.keys.len());
                frame.keys.push(key);
                self.work.push(Work::Finite(FiniteWork::Select(frame)));
            }
            FiniteWork::Merge(mut frame) => {
                let n = frame.order.len();
                if frame.width >= n {
                    frame.base = 0;
                    self.work.push(Work::Finite(FiniteWork::Output(frame)));
                } else if frame.base >= n {
                    std::mem::swap(&mut frame.order, &mut frame.scratch);
                    frame.scratch.clear();
                    frame.base = 0;
                    frame.end = 0;
                    frame.width = frame.width.saturating_mul(2);
                    self.work.push(Work::Finite(FiniteWork::Merge(frame)));
                } else {
                    if frame.end <= frame.base {
                        frame.left = frame.base;
                        frame.middle = frame.base.saturating_add(frame.width).min(n);
                        frame.right = frame.middle;
                        frame.end = frame.middle.saturating_add(frame.width).min(n);
                    }
                    if frame.left == frame.middle && frame.right == frame.end {
                        frame.base = frame.end;
                    } else {
                        let take_left = frame.right == frame.end
                            || frame.left < frame.middle
                                && self.compare_order(
                                    &frame.keys[frame.order[frame.left]],
                                    &frame.keys[frame.order[frame.right]],
                                    frame.span,
                                    "sortBy keys",
                                )? != Ordering::Greater;
                        let index = if take_left {
                            let index = frame.left;
                            frame.left += 1;
                            index
                        } else {
                            let index = frame.right;
                            frame.right += 1;
                            index
                        };
                        frame.scratch.push(frame.order[index]);
                    }
                    self.work.push(Work::Finite(FiniteWork::Merge(frame)));
                }
            }
            FiniteWork::Output(mut frame) => {
                if let Some(index) = frame.order.get(frame.base) {
                    frame.output.push(frame.items[*index].clone());
                    frame.base += 1;
                    self.work.push(Work::Finite(FiniteWork::Output(frame)));
                } else {
                    let value = Item::List(Arc::new(frame.output));
                    self.values.push(if let Some(shape) = frame.output_type {
                        value.typed(shape)
                    } else {
                        value
                    });
                }
            }
        }
        Ok(())
    }
    /// Empty lists and absent Options leave only their element undetermined. Heterogeneous
    /// actual values fail here; they are never laundered through a List<Unknown> declaration.
    fn collection_shape(
        &mut self,
        value: &Item,
        span: Span,
        depth: usize,
    ) -> Result<Shape, Failure> {
        if depth > 128 {
            return Err(Failure::new(
                "CAL006",
                span,
                "concat element nesting exceeds 128",
            ));
        }
        self.budget.work(1, span)?;
        self.budget.allocate(192, span)?;
        Ok(match value.untyped() {
            Item::Scalar(data) => super::super::value::shape(data, 0),
            Item::Option(None) => Shape::Option(Box::new(Shape::Unknown)),
            Item::Option(Some(v)) => {
                Shape::Option(Box::new(self.collection_shape(v, span, depth + 1)?))
            }
            Item::List(values) => {
                let mut shape = Shape::Unknown;
                for value in values.iter() {
                    let next = self.collection_shape(value, span, depth + 1)?;
                    shape = merge_collection_shapes(&shape, &next).ok_or_else(|| {
                        Failure::new("CAL004", span, "concat requires homogeneous nested lists")
                    })?;
                }
                Shape::List(Box::new(shape))
            }
            Item::Record(fields) => {
                let mut result = Vec::with_capacity(fields.len());
                for (name, value) in fields.iter() {
                    self.budget.work(name.len() as u64, span)?;
                    self.budget.allocate(name.len() as u64, span)?;
                    result.push((name.clone(), self.collection_shape(value, span, depth + 1)?));
                }
                Shape::Record(RecordShape::new("", result).expect("distinct fields"))
            }
            _ => {
                return Err(Failure::new(
                    "CAL004",
                    span,
                    "concat elements must be finite data values",
                ));
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merge_pass_itself_yields_and_remains_cancellable() {
        let program = wes_language::calc::parse_body(
            "return range(512).sortBy(x=>0-x);",
            0,
            wes_language::calc::Package::standard(),
        )
        .unwrap();
        let compiled = wes_language::calc::analyze(
            Arc::new(program),
            wes_language::calc::Environment {
                catalogue: &wes_core::capability::Catalogue::new(),
                contracts: &wes_core::contracts::ContractRegistry::new(),
                workspace: &|_| None,
            },
        )
        .unwrap();
        let mut machine = Machine::new(
            Arc::new(compiled),
            Default::default(),
            Limits {
                quantum: 1,
                ..Limits::default()
            },
        )
        .unwrap();
        let token = CancellationToken::new();
        let mut found = false;
        for _ in 0..20_000 {
            assert!(matches!(machine.poll(&token).unwrap(), Step::Yield));
            if matches!(machine.work.last(), Some(Work::Finite(FiniteWork::Merge(frame))) if frame.width >= 4)
            {
                found = true;
                break;
            }
        }
        assert!(found, "did not reach a later merge pass");
        token.cancel();
        assert!(machine.poll(&token).unwrap_err().cancelled);
        assert!(machine.poll(&CancellationToken::new()).is_err());
    }
}
