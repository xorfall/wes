//! Pure finite transforms share the machine's yielding work and allocation budget.
use super::*;
use wes_core::Shape;

pub(super) enum Utility {
    Join {
        items: Arc<Vec<Item>>,
        separator: String,
        index: usize,
        output: String,
        span: Span,
    },
    Fields {
        records: [Arc<IndexMap<String, Item>>; 2],
        index: usize,
        output: IndexMap<String, Item>,
        span: Span,
    },
    ListSlice {
        items: Arc<Vec<Item>>,
        index: usize,
        end: usize,
        output: Vec<Item>,
        shape: Option<Shape>,
        span: Span,
    },
    TextSlice {
        source: Item,
        start: usize,
        end: Option<usize>,
        character: usize,
        byte: usize,
        begin: Option<usize>,
        span: Span,
    },
}
impl Machine {
    pub(super) fn start_utility(
        &mut self,
        operation: Operation,
        args: Vec<Item>,
        span: Span,
    ) -> Result<(), Failure> {
        let work = match operation {
            Operation::Join => {
                let separator = args[1].text(span)?;
                self.budget.allocate(separator.len() as u64, span)?;
                Utility::Join {
                    items: args[0].list(span)?,
                    separator: separator.to_owned(),
                    index: 0,
                    output: String::new(),
                    span,
                }
            }
            Operation::WithFields => {
                let record = |value: &Item| match value.untyped() {
                    Item::Record(fields) => Ok(fields.clone()),
                    _ => Err(value.expected("Record", span)),
                };
                Utility::Fields {
                    records: [record(&args[0])?, record(&args[1])?],
                    index: 0,
                    output: IndexMap::new(),
                    span,
                }
            }
            Operation::Slice => {
                let index = |value: &Item| {
                    usize::try_from(value.int(span)?).map_err(|_| {
                        Failure::new(
                            wes_language::calc::diagnostics::Category::Bounds.code(),
                            span,
                            "slice indices must be nonnegative Int values",
                        )
                    })
                };
                let start = index(&args[1])?;
                let end = args.get(2).map(index).transpose()?;
                if end.is_some_and(|end| start > end) {
                    return Err(Failure::new(
                        wes_language::calc::diagnostics::Category::Bounds.code(),
                        span,
                        "slice start must not exceed end",
                    ));
                }
                match args[0].untyped() {
                    Item::List(items) => {
                        let end = end.unwrap_or(items.len());
                        if start > end || end > items.len() {
                            return Err(Failure::new(
                                wes_language::calc::diagnostics::Category::Bounds.code(),
                                span,
                                "slice indices exceed List length",
                            ));
                        }
                        Utility::ListSlice {
                            items: items.clone(),
                            index: start,
                            end,
                            output: vec![],
                            shape: match &args[0] {
                                Item::Typed(_, shape, _) => Some(shape.as_ref().clone()),
                                _ => None,
                            },
                            span,
                        }
                    }
                    Item::Scalar(data) if matches!(data.as_ref(), Data::Text(_)) => {
                        Utility::TextSlice {
                            source: args[0].clone(),
                            start,
                            end,
                            character: 0,
                            byte: 0,
                            begin: None,
                            span,
                        }
                    }
                    _ => return Err(args[0].expected("Text or List", span)),
                }
            }
            _ => unreachable!("finite utility dispatch"),
        };
        self.work.push(Work::Utility(work));
        Ok(())
    }
    pub(super) fn utility_work(&mut self, work: Utility) -> Result<(), Failure> {
        match work {
            Utility::Join {
                items,
                separator,
                mut index,
                mut output,
                span,
            } => {
                if let Some(item) = items.get(index) {
                    let text = item.text(span)?;
                    let extra = if index > 0 { separator.len() } else { 0 };
                    self.budget.work(text.len() as u64 + extra as u64, span)?;
                    self.budget
                        .allocate(text.len() as u64 + extra as u64, span)?;
                    if index > 0 {
                        output.push_str(&separator);
                    }
                    output.push_str(text);
                    index += 1;
                    self.work.push(Work::Utility(Utility::Join {
                        items,
                        separator,
                        index,
                        output,
                        span,
                    }));
                } else {
                    self.values.push(Item::scalar(Data::Text(output.into())));
                }
            }
            Utility::Fields {
                records,
                mut index,
                mut output,
                span,
            } => {
                let entry = if index < records[0].len() {
                    records[0].get_index(index)
                } else {
                    records[1].get_index(index - records[0].len())
                };
                if let Some((name, value)) = entry {
                    self.budget.work(name.len() as u64 + 1, span)?;
                    self.budget.allocate(name.len() as u64 + 96, span)?;
                    output.insert(name.clone(), value.clone());
                    index += 1;
                    self.work.push(Work::Utility(Utility::Fields {
                        records,
                        index,
                        output,
                        span,
                    }));
                } else {
                    self.values.push(Item::Record(Arc::new(output)));
                }
            }
            Utility::ListSlice {
                items,
                mut index,
                end,
                mut output,
                shape,
                span,
            } => {
                if index < end {
                    self.budget.allocate(96, span)?;
                    output.push(items[index].clone());
                    index += 1;
                    self.work.push(Work::Utility(Utility::ListSlice {
                        items,
                        index,
                        end,
                        output,
                        shape,
                        span,
                    }));
                } else {
                    let result = Item::List(Arc::new(output));
                    self.values.push(if let Some(shape) = shape {
                        result.typed(shape)
                    } else {
                        result
                    });
                }
            }
            Utility::TextSlice {
                source,
                start,
                end,
                mut character,
                mut byte,
                mut begin,
                span,
            } => {
                let text = source.text(span)?;
                if character == start {
                    begin = Some(byte);
                }
                if end == Some(character) || byte == text.len() {
                    let begin = begin.ok_or_else(|| {
                        Failure::new(
                            wes_language::calc::diagnostics::Category::Bounds.code(),
                            span,
                            format!(
                                "slice start {start} exceeds Text character length {character}"
                            ),
                        )
                    })?;
                    if end.is_some_and(|end| end > character) {
                        return Err(Failure::new(
                            wes_language::calc::diagnostics::Category::Bounds.code(),
                            span,
                            format!(
                                "slice end {} exceeds Text character length {character}",
                                end.expect("requested end")
                            ),
                        ));
                    }
                    self.budget.allocate((byte - begin) as u64, span)?;
                    self.budget.work((byte - begin) as u64, span)?;
                    self.values
                        .push(Item::scalar(Data::Text(text[begin..byte].into())));
                } else {
                    let width = text[byte..]
                        .chars()
                        .next()
                        .expect("character boundary")
                        .len_utf8();
                    self.budget.work(width as u64, span)?;
                    byte += width;
                    character += 1;
                    self.work.push(Work::Utility(Utility::TextSlice {
                        source,
                        start,
                        end,
                        character,
                        byte,
                        begin,
                        span,
                    }));
                }
            }
        }
        Ok(())
    }
}
