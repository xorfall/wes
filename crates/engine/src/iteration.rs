//! Independent bounded source cursors. No provider, graph mutation, or durable cursor state.
use crate::{calc::Failure, driver::CancellationToken};
use std::sync::Arc;
use wes_core::{Data, IterMode, IterValue};
use wes_language::Span;

#[derive(Debug, PartialEq, Eq)]
pub enum CursorItem {
    Item {
        data: Data,
        index: u64,
        offset: Option<usize>,
    },
    Json {
        text: String,
        index: u64,
        offset: usize,
    },
    End,
}
/// The plan owns a source snapshot. Each cursor owns only its independent position and regex state.
pub struct SourceCursor {
    plan: Arc<IterValue>,
    position: usize,
    index: u64,
    ended: bool,
    failed: Option<Failure>,
    regex: Option<Arc<regex::Regex>>,
    search: usize,
}
impl SourceCursor {
    pub fn new(plan: Arc<IterValue>) -> Result<Self, Failure> {
        let regex = plan
            .compiled_regex()
            .map_err(|e| Failure::new("CAL004", Span::at(0), e))?;
        Ok(Self {
            plan,
            position: 0,
            index: 0,
            ended: false,
            failed: None,
            regex,
            search: 0,
        })
    }
    pub fn next_raw(
        &mut self,
        token: &CancellationToken,
        charge: &mut dyn FnMut(u64) -> Result<(), Failure>,
    ) -> Result<CursorItem, Failure> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        if self.ended {
            return Ok(CursorItem::End);
        }
        let result = self.next_inner(token, charge);
        if let Err(error) = &result {
            self.failed = Some(error.clone());
        }
        if matches!(result, Ok(CursorItem::End)) {
            self.ended = true;
        }
        result
    }
    fn next_inner(
        &mut self,
        token: &CancellationToken,
        charge: &mut dyn FnMut(u64) -> Result<(), Failure>,
    ) -> Result<CursorItem, Failure> {
        if token.is_cancelled() {
            return Err(Failure::cancelled(Span::at(0)));
        }
        charge(1)?;
        let index = self.index;
        self.index = self
            .index
            .checked_add(1)
            .ok_or_else(|| Failure::new("CAL006", Span::at(0), "Iter position exhausted"))?;
        let data = match self.plan.source().data() {
            Data::List(items) => {
                let Some(item) = items.get(self.position) else {
                    return Ok(CursorItem::End);
                };
                charge(data_work(item)?)?;
                self.position += 1;
                return Ok(CursorItem::Item {
                    data: item.clone(),
                    index,
                    offset: None,
                });
            }
            Data::Record(fields) => {
                let Some((key, value)) = fields.get_index(self.position) else {
                    return Ok(CursorItem::End);
                };
                self.position += 1;
                match self.plan.mode() {
                    IterMode::Keys => {
                        charge(key.len() as u64)?;
                        Data::Text(key.as_str().into())
                    }
                    IterMode::Values => {
                        charge(data_work(value)?)?;
                        value.clone()
                    }
                    IterMode::Entries => {
                        charge(data_work(value)?.saturating_add(key.len() as u64))?;
                        Data::Record(
                            [
                                ("key".into(), Data::Text(key.as_str().into())),
                                ("value".into(), value.clone()),
                            ]
                            .into(),
                        )
                    }
                    _ => {
                        return Err(Failure::new(
                            "CAL004",
                            Span::at(0),
                            "invalid record traversal",
                        ));
                    }
                }
            }
            Data::Text(text) => {
                let start = self.position;
                let range = match self.plan.mode() {
                    IterMode::Chars => {
                        let Some(c) = text[start..].chars().next() else {
                            return Ok(CursorItem::End);
                        };
                        charge(c.len_utf8() as u64)?;
                        self.position += c.len_utf8();
                        start..self.position
                    }
                    IterMode::Lines | IterMode::JsonLines => {
                        if start == text.len() {
                            return Ok(CursorItem::End);
                        }
                        let mut end = start;
                        for byte in text[start..].bytes() {
                            charge(1)?;
                            if byte == b'\n' {
                                break;
                            }
                            end += 1;
                        }
                        self.position = if end < text.len() { end + 1 } else { end };
                        let end =
                            if end < text.len() && end > start && text.as_bytes()[end - 1] == b'\r'
                            {
                                end - 1
                            } else {
                                end
                            };
                        start..end
                    }
                    IterMode::Words => {
                        let mut begin = start;
                        let mut end = text.len();
                        let mut found = false;
                        for (offset, c) in text[start..].char_indices() {
                            charge(c.len_utf8() as u64)?;
                            if !found {
                                if c.is_whitespace() {
                                    begin = start + offset + c.len_utf8();
                                } else {
                                    found = true;
                                }
                            } else if c.is_whitespace() {
                                end = start + offset;
                                break;
                            }
                        }
                        if !found {
                            return Ok(CursorItem::End);
                        }
                        self.position = end;
                        begin..end
                    }
                    IterMode::Split => {
                        if start > text.len() {
                            return Ok(CursorItem::End);
                        }
                        let delimiter = self.plan.argument().expect("validated delimiter");
                        // Charge before the library search, bounding even an unsuccessful scan.
                        charge((text.len() - start) as u64)?;
                        if let Some(at) = text[start..].find(delimiter) {
                            self.position = start + at + delimiter.len();
                            start..start + at
                        } else {
                            self.position = text.len() + 1;
                            start..text.len()
                        }
                    }
                    IterMode::Captures => {
                        if self.search > text.len() {
                            return Ok(CursorItem::End);
                        }
                        charge((text.len() - self.search) as u64)?;
                        let regex = self.regex.as_ref().expect("compiled regex");
                        let Some(captures) = regex.captures_at(text, self.search) else {
                            return Ok(CursorItem::End);
                        };
                        let matched = captures.get(0).expect("whole match");
                        self.search = if matched.start() == matched.end() {
                            text[matched.end()..]
                                .chars()
                                .next()
                                .map_or(text.len() + 1, |c| matched.end() + c.len_utf8())
                        } else {
                            matched.end()
                        };
                        self.position = matched.end();
                        charge(captures.len() as u64 + matched.len() as u64)?;
                        let groups = captures
                            .iter()
                            .skip(1)
                            .map(|capture| {
                                if let Some(capture) = capture {
                                    charge(capture.len() as u64)?;
                                }
                                Ok(Data::Option(capture.map(|capture| {
                                    Box::new(Data::Text(capture.as_str().into()))
                                })))
                            })
                            .collect::<Result<Vec<_>, Failure>>()?;
                        return Ok(CursorItem::Item {
                            data: Data::Record(
                                [
                                    ("match".into(), Data::Text(matched.as_str().into())),
                                    ("groups".into(), Data::List(groups)),
                                ]
                                .into(),
                            ),
                            index,
                            offset: Some(matched.start()),
                        });
                    }
                    IterMode::Matches | IterMode::RegexSplit => {
                        if self.search > text.len() {
                            if self.plan.mode() == IterMode::RegexSplit
                                && self.position <= text.len()
                            {
                                self.position = text.len() + 1;
                                start..text.len()
                            } else {
                                return Ok(CursorItem::End);
                            }
                        } else {
                            charge((text.len() - self.search) as u64)?;
                            let found = self
                                .regex
                                .as_ref()
                                .expect("compiled regex")
                                .find_at(text, self.search);
                            if let Some(m) = found {
                                self.search = if m.start() == m.end() {
                                    text[m.end()..]
                                        .chars()
                                        .next()
                                        .map_or(text.len() + 1, |c| m.end() + c.len_utf8())
                                } else {
                                    m.end()
                                };
                                self.position = m.end();
                                if self.plan.mode() == IterMode::Matches {
                                    m.start()..m.end()
                                } else {
                                    start..m.start()
                                }
                            } else {
                                self.search = text.len() + 1;
                                if self.plan.mode() == IterMode::Matches {
                                    return Ok(CursorItem::End);
                                }
                                self.position = text.len() + 1;
                                start..text.len()
                            }
                        }
                    }
                    _ => {
                        return Err(Failure::new(
                            "CAL004",
                            Span::at(0),
                            "invalid text traversal",
                        ));
                    }
                };
                charge(range.len() as u64)?;
                let value = text[range.clone()].to_owned();
                if self.plan.mode() == IterMode::JsonLines {
                    return Ok(CursorItem::Json {
                        text: value,
                        index,
                        offset: range.start,
                    });
                }
                return Ok(CursorItem::Item {
                    data: Data::Text(value.into()),
                    index,
                    offset: Some(range.start),
                });
            }
            _ => return Err(Failure::new("CAL004", Span::at(0), "invalid Iter source")),
        };
        Ok(CursorItem::Item {
            data,
            index,
            offset: None,
        })
    }
}
fn data_work(data: &Data) -> Result<u64, Failure> {
    let mut count = 0u64;
    let mut pending = vec![(data, 0)];
    while let Some((data, depth)) = pending.pop() {
        count += 1;
        if depth > 128 || count > 1_000_000 || pending.len() > 100_000 {
            return Err(Failure::new(
                "CAL006",
                Span::at(0),
                "Iter item exceeds limits",
            ));
        }
        match data {
            Data::Text(s) => count = count.saturating_add(s.len() as u64),
            Data::Bytes(s) => count = count.saturating_add(s.len() as u64),
            Data::Decimal(d) => count = count.saturating_add(d.compact_text_size_bound()),
            Data::List(xs) => {
                if xs.len() > 100_000 {
                    return Err(Failure::new("CAL006", Span::at(0), "Iter item too wide"));
                }
                pending.extend(xs.iter().map(|x| (x, depth + 1)));
            }
            Data::Record(xs) => {
                if xs.len() > 100_000 {
                    return Err(Failure::new("CAL006", Span::at(0), "Iter item too wide"));
                }
                pending.extend(xs.values().map(|x| (x, depth + 1)));
                count = count.saturating_add(xs.keys().map(|s| s.len() as u64).sum::<u64>());
            }
            Data::Option(Some(x)) => pending.push((x, depth + 1)),
            Data::Iter(_) => {
                return Err(Failure::new(
                    "CAL004",
                    Span::at(0),
                    "nested Iter source items require explicit collection",
                ));
            }
            _ => {}
        }
    }
    Ok(count)
}
