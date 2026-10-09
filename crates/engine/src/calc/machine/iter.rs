use super::*;
use crate::{
    calc::iteration::{Cursor, Pipeline, Stage},
    iteration::CursorItem,
};
use wes_core::{IterMode, IterStage, IterValue};

pub(super) enum Consumer {
    Collect(Vec<Item>),
    Count(i64),
    Reduce {
        callback: Item,
        accumulator: Item,
    },
    For {
        symbol: Symbol,
        body: StmtId,
        env: usize,
    },
}
pub(super) struct Frame {
    cursor: Cursor,
    consumer: Consumer,
    span: Span,
}
pub(super) enum IterWork {
    Pull(Box<Frame>),
    Stage(Box<Frame>, usize, Item),
    Json(Box<Frame>),
    Callback(Box<Frame>, usize, Option<Item>),
    Reduced(Box<Frame>),
}
impl Machine {
    pub(super) fn start_iter(
        &mut self,
        pipeline: Arc<Pipeline>,
        consumer: Consumer,
        span: Span,
    ) -> Result<(), Failure> {
        let metadata = pipeline
            .stages
            .iter()
            .map(|s| match s {
                Stage::Data(IterStage::Check(c)) => {
                    c.packages.iter().map(|s| s.len() as u64).sum::<u64>()
                }
                _ => 128,
            })
            .sum::<u64>();
        self.budget.work(
            metadata + pipeline.root.argument().map_or(0, |s| s.len() as u64),
            span,
        )?;
        self.budget.allocate(metadata + 512, span)?;
        if matches!(
            pipeline.root.mode(),
            wes_core::IterMode::RegexSplit
                | wes_core::IterMode::Matches
                | wes_core::IterMode::Captures
        ) {
            self.admit_regex(pipeline.root.argument().expect("regex argument"), span)?;
        }
        let cursor = Cursor::new(pipeline, span, &mut self.iter_regex)?;
        self.work.push(Work::Iter(IterWork::Pull(Box::new(Frame {
            cursor,
            consumer,
            span,
        }))));
        Ok(())
    }
    pub(super) fn iter_operation(
        &mut self,
        op: Operation,
        args: &[Item],
        expression: ExprId,
        span: Span,
        token: &CancellationToken,
    ) -> Result<bool, Failure> {
        use Operation::*;
        match op {
            Map | Filter | Reduce if matches!(args[0].untyped(), Item::Iter(_)) => {
                let pipeline = args[0].iter(span)?;
                let callback = args[1].clone();
                if op == Reduce {
                    self.start_iter(
                        pipeline,
                        Consumer::Reduce {
                            callback,
                            accumulator: args[2].clone(),
                        },
                        span,
                    )?;
                } else {
                    let Item::Function { function, .. } = callback.untyped() else {
                        return Err(Failure::new(
                            "CAL004",
                            span,
                            "lazy Iter callbacks require a statically verified function",
                        ));
                    };
                    if !self.compiled.pure_callback(*function) {
                        return Err(Failure::new(
                            "CAL009",
                            span,
                            "lazy Iter callbacks must be pure and cannot capture mutable outer bindings; use for-of for effects",
                        ));
                    }
                    let stage = if op == Map {
                        Stage::Map(callback)
                    } else {
                        Stage::Filter(callback)
                    };
                    self.budget
                        .allocate(128 * (pipeline.stages.len() as u64 + 1), span)?;
                    self.values
                        .push(Item::Iter(Arc::new(pipeline.push(stage, span)?)));
                }
            }
            Collect | Count => self.start_iter(
                args[0].iter(span)?,
                if op == Collect {
                    Consumer::Collect(vec![])
                } else {
                    Consumer::Count(0)
                },
                span,
            )?,
            Take | Skip | Field | IterChecked => {
                let pipeline = args[0].iter(span)?;
                let stage = match op {
                    Take | Skip => {
                        let received = args[1].int(span)?;
                        let n = u64::try_from(received).map_err(|_| {
                            Failure::categorized(
                                wes_language::calc::diagnostics::Category::Bounds,
                                span,
                                format!(
                                    "{} count must be nonnegative; received {received}",
                                    op.id()
                                ),
                            )
                        })?;
                        if op == Take {
                            IterStage::Take(n)
                        } else {
                            IterStage::Skip(n)
                        }
                    }
                    Field => {
                        let name = args[1].text(span)?;
                        if name.len() > 4096 {
                            return Err(Failure::new("CAL006", span, "Iter field name too long"));
                        }
                        IterStage::Field(name.to_owned())
                    }
                    IterChecked => IterStage::Check(
                        self.compiled
                            .iter_contracts
                            .get(&expression)
                            .cloned()
                            .ok_or_else(|| {
                                Failure::new("CAL002", span, "uncaptured Iter contract")
                            })?,
                    ),
                    _ => unreachable!(),
                };
                self.budget
                    .allocate(128 * (pipeline.stages.len() as u64 + 1), span)?;
                self.values.push(Item::Iter(Arc::new(
                    pipeline.push(Stage::Data(stage), span)?,
                )));
            }
            IterItems | IterLines | IterChars | IterWords | IterSplit | IterRegexSplit
            | IterMatches | IterCaptures | IterKeys | IterValues | IterEntries | IterJsonLines
            | IterUse => {
                let (source, mode, argument, stages) = if op == IterUse {
                    let (recipe, capture) = self
                        .compiled
                        .iter_recipes
                        .get(&expression)
                        .cloned()
                        .ok_or_else(|| {
                            Failure::new("CAL002", span, "uncaptured iterator recipe")
                        })?;
                    let source = self.iter_source(&args[1], span)?;
                    let issues = recipe
                        .input
                        .issues_with_cancel(source.data(), &|| token.is_cancelled())
                        .map_err(|_| Failure::cancelled(span))?;
                    if !issues.is_empty() {
                        return Err(Failure {
                            issues,
                            ..Failure::new(
                                wes_language::calc::diagnostics::Category::Contract.code(),
                                span,
                                "iterator recipe input validation failed",
                            )
                        });
                    }
                    (
                        source,
                        recipe.mode,
                        recipe.argument,
                        vec![IterStage::Check(capture)],
                    )
                } else {
                    let source = self.iter_source(&args[0], span)?;
                    let mode = op.iter_mode().expect("iterator constructor");
                    let arg = args
                        .get(1)
                        .map(|a| a.text(span).map(str::to_owned))
                        .transpose()?;
                    (source, mode, arg, vec![])
                };
                self.budget
                    .work(argument.as_ref().map_or(1, |s| s.len() as u64), span)?;
                if matches!(
                    mode,
                    wes_core::IterMode::RegexSplit
                        | wes_core::IterMode::Matches
                        | wes_core::IterMode::Captures
                ) {
                    self.admit_regex(argument.as_deref().expect("regex argument"), span)?;
                }
                let plan =
                    IterValue::new_cached(source, mode, argument, stages, &mut self.iter_regex)
                        .map_err(|e| Failure::iteration(e, span))?;
                self.values
                    .push(Item::Iter(Arc::new(Pipeline::from_value(Arc::new(plan)))));
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
    fn iter_source(&mut self, item: &Item, span: Span) -> Result<Value, Failure> {
        let data = item.data(&mut self.budget, span, 0)?;
        Value::new(item.output_shape(&data), data, self.provenance.clone())
            .map_err(|_| Failure::new("CAL004", span, "invalid Iter source"))
    }
    pub(super) fn iter_work(
        &mut self,
        work: IterWork,
        token: &CancellationToken,
    ) -> Result<Option<Request>, Failure> {
        match work {
            IterWork::Pull(mut frame) => {
                let span = frame.span;
                if frame.cursor.finished() {
                    self.iter_end(frame);
                    return Ok(None);
                }
                let raw = frame
                    .cursor
                    .source
                    .next_raw(token, &mut |n| self.budget.work(n, span))?;
                match raw {
                    CursorItem::End => self.iter_end(frame),
                    CursorItem::Item {
                        data,
                        index,
                        offset,
                    } => {
                        frame.cursor.index = index;
                        frame.cursor.offset = offset;
                        if frame.cursor.pipeline.root.mode() == IterMode::Items
                            && let wes_core::Shape::List(element) =
                                frame.cursor.pipeline.root.source().shape()
                            && !super::super::host::fits(&data, element, token, 0)
                        {
                            return Err(self.iter_error(
                                &frame,
                                "source item does not satisfy its declared shape",
                            ));
                        }
                        let item = Item::Typed(
                            Arc::new(Item::from_data(&data, &mut self.budget, span, 0)?),
                            frame.cursor.source_shape.clone(),
                            None,
                        );
                        self.work.push(Work::Iter(IterWork::Stage(frame, 0, item)));
                    }
                    CursorItem::Json {
                        text,
                        index,
                        offset,
                    } => {
                        frame.cursor.index = index;
                        frame.cursor.offset = Some(offset);
                        if text.trim().is_empty() {
                            return Err(Failure::categorized(
                                wes_language::calc::diagnostics::Category::Parse,
                                span,
                                format!(
                                    "JSONL line {} is blank; every line must contain JSON. To skip blank lines explicitly, use iter.lines(source).filter(line => iter.words(line).count() > 0).map(line => parseJson(line)).collect()",
                                    index + 1
                                ),
                            ));
                        }
                        let mut contract = None;
                        for (i, stage) in frame.cursor.pipeline.stages.iter().enumerate() {
                            match stage {
                                Stage::Data(IterStage::Take(_) | IterStage::Skip(_)) => {}
                                Stage::Data(IterStage::Check(_)) => {
                                    contract = frame.cursor.checks[i].clone();
                                    break;
                                }
                                _ => break,
                            }
                        }
                        self.budget.allocate(text.len() as u64, span)?;
                        self.work.push(Work::Iter(IterWork::Json(frame)));
                        let id = self.suspend()?;
                        self.pending_origin = false;
                        return Ok(Some(Request::Json {
                            id,
                            bytes: text.into_bytes(),
                            mode: super::super::JsonMode::Parse,
                            contract,
                            span,
                        }));
                    }
                }
            }
            IterWork::Json(frame) => {
                let item = self.pop()?;
                self.work.push(Work::Iter(IterWork::Stage(frame, 0, item)));
            }
            IterWork::Stage(mut frame, index, mut item) => {
                let span = frame.span;
                let Some(stage) = frame.cursor.pipeline.stages.get(index).cloned() else {
                    self.iter_deliver(frame, item, token)?;
                    return Ok(None);
                };
                match stage {
                    Stage::Data(IterStage::Take(_)) => frame.cursor.counts[index] += 1,
                    Stage::Data(IterStage::Skip(n)) => {
                        if frame.cursor.counts[index] < n {
                            frame.cursor.counts[index] += 1;
                            self.work.push(Work::Iter(IterWork::Pull(frame)));
                            return Ok(None);
                        }
                    }
                    Stage::Data(IterStage::Field(name)) => {
                        let Item::Record(fields) = item.untyped() else {
                            return Err(
                                self.iter_error(&frame, "field projection requires a record")
                            );
                        };
                        let field = fields
                            .get(&name)
                            .cloned()
                            .ok_or_else(|| self.iter_error(&frame, "projected field is absent"))?;
                        item = item.project_field_shape(field, &name);
                    }
                    Stage::Data(IterStage::Check(_)) => {
                        let contract = frame.cursor.checks[index]
                            .as_ref()
                            .expect("captured contract");
                        let data = item.data(&mut self.budget, span, 0)?;
                        let issues = contract
                            .issues_with_cancel(&data, &|| token.is_cancelled())
                            .map_err(|_| Failure::cancelled(span))?;
                        if !issues.is_empty() {
                            return Err(Failure {
                                code: wes_language::calc::diagnostics::Category::Contract.code(),
                                issues,
                                ..self.iter_error(&frame, "item contract validation failed")
                            });
                        }
                        item = item.typed(contract.shape());
                    }
                    Stage::Map(callback) | Stage::Filter(callback) => {
                        let original =
                            if matches!(frame.cursor.pipeline.stages[index], Stage::Filter(_)) {
                                Some(item.clone())
                            } else {
                                None
                            };
                        self.work
                            .push(Work::Iter(IterWork::Callback(frame, index + 1, original)));
                        if self
                            .invoke(callback, vec![item], usize::MAX, span, token)?
                            .is_some()
                        {
                            return Err(Failure::new(
                                "CAL002",
                                span,
                                "uncaptured Iter callback operation",
                            ));
                        }
                        return Ok(None);
                    }
                }
                self.work
                    .push(Work::Iter(IterWork::Stage(frame, index + 1, item)));
            }
            IterWork::Callback(frame, index, original) => {
                let result = self.pop()?;
                if let Some(item) = original {
                    if result.bool(frame.span)? {
                        self.work
                            .push(Work::Iter(IterWork::Stage(frame, index, item)));
                    } else {
                        self.work.push(Work::Iter(IterWork::Pull(frame)));
                    }
                } else {
                    self.work
                        .push(Work::Iter(IterWork::Stage(frame, index, result)));
                }
            }
            IterWork::Reduced(mut frame) => {
                let result = self.pop()?;
                let Consumer::Reduce { accumulator, .. } = &mut frame.consumer else {
                    unreachable!()
                };
                *accumulator = result;
                self.work.push(Work::Iter(IterWork::Pull(frame)));
            }
        }
        Ok(None)
    }
    fn iter_error(&self, frame: &Frame, message: &str) -> Failure {
        Failure::new(
            "CAL004",
            frame.span,
            format!(
                "Iter item {}{}: {message}",
                frame.cursor.index,
                frame
                    .cursor
                    .offset
                    .map(|n| format!(", byte {n}"))
                    .unwrap_or_default()
            ),
        )
    }
    fn iter_end(&mut self, frame: Box<Frame>) {
        match frame.consumer {
            Consumer::Collect(items) => {
                let shape = frame.cursor.output_shape();
                let value = Item::List(Arc::new(items));
                self.values.push(if shape == wes_core::Shape::Unknown {
                    value
                } else {
                    value.typed(wes_core::Shape::List(Box::new(shape)))
                });
            }
            Consumer::Count(n) => self.values.push(Item::scalar(Data::Int(n))),
            Consumer::Reduce { accumulator, .. } => self.values.push(accumulator),
            Consumer::For { .. } => {}
        }
    }
    fn iter_deliver(
        &mut self,
        mut frame: Box<Frame>,
        item: Item,
        token: &CancellationToken,
    ) -> Result<(), Failure> {
        let span = frame.span;
        match &mut frame.consumer {
            Consumer::Collect(items) => {
                self.budget.allocate(96, span)?;
                items.push(item);
            }
            Consumer::Count(n) => {
                *n = n
                    .checked_add(1)
                    .ok_or_else(|| Failure::new("CAL006", span, "Iter count overflow"))?
            }
            Consumer::Reduce {
                callback,
                accumulator,
            } => {
                let callback = callback.clone();
                let args = vec![accumulator.clone(), item];
                self.work.push(Work::Iter(IterWork::Reduced(frame)));
                if self
                    .invoke(callback, args, usize::MAX, span, token)?
                    .is_some()
                {
                    return Err(Failure::new(
                        "CAL002",
                        span,
                        "metadata requires a direct call",
                    ));
                }
                return Ok(());
            }
            Consumer::For { symbol, body, env } => {
                let inner = self.env(*env)?;
                self.declare(inner, *symbol, Some(item))?;
                let body = *body;
                self.work.push(Work::Loop(Loop::Iter(frame)));
                self.work.push(Work::Statement(body, inner));
                return Ok(());
            }
        }
        self.work.push(Work::Iter(IterWork::Pull(frame)));
        Ok(())
    }
}

impl IterWork {
    pub(super) fn location(&self) -> String {
        let frame = match self {
            Self::Pull(f)
            | Self::Stage(f, ..)
            | Self::Json(f)
            | Self::Callback(f, ..)
            | Self::Reduced(f) => f,
        };
        format!(
            "Iter item {}{}",
            frame.cursor.index,
            frame
                .cursor
                .offset
                .map(|n| format!(", byte {n}"))
                .unwrap_or_default()
        )
    }
}
