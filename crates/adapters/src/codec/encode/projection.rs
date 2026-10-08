//! Read-side projection borrows the selected immutable data. It neither evaluates
//! expressions nor clones/serializes unselected bodies. All egress keeps root policy.
use super::*;
use serde::Deserialize;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValueSelection {
    pub select: Option<String>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
    #[serde(default)]
    pub shape_only: bool,
}
impl ValueSelection {
    pub fn is_requested(&self) -> bool {
        self.select.is_some() || self.offset.is_some() || self.limit.is_some() || self.shape_only
    }
}
fn invalid(text: &str) -> CodecError {
    CodecError::Selection(text.into())
}

pub fn encode_selection(
    value: &Value,
    selection: &ValueSelection,
    typed: bool,
    limits: Limits,
) -> Result<Vec<u8>, CodecError> {
    let policy = value.provenance().policy();
    // Check before even resolving a path: a path error or length is also an export.
    if policy.is_confidential() || policy.is_unknown() {
        return Err(CodecError::Export(
            "This value cannot be exported to terminal processes.".into(),
        ));
    }
    let pointer = selection.select.as_deref().unwrap_or("");
    let (data, shape, path) = select(value, pointer)?;
    let meta = if typed {
        value.metadata().and_then(|m| m.project(&path))
    } else {
        None
    };
    let paging = selection.offset.is_some() || selection.limit.is_some();
    if selection.shape_only && paging {
        return Err(invalid("shape_only cannot be combined with offset/limit."));
    }
    let offset = selection.offset.unwrap_or(0);
    let limit = selection.limit.unwrap_or(50);
    if paging && !(1..=1000).contains(&limit) {
        return Err(invalid("limit must be between 1 and 1000."));
    }
    let (body, page) = if paging {
        let Data::List(items) = data else {
            return Err(invalid("offset/limit requires a selected list."));
        };
        if offset > items.len() {
            return Err(invalid("offset exceeds the selected list length."));
        }
        let end = offset.saturating_add(limit).min(items.len());
        (
            BorrowedData::List(&items[offset..end]),
            Some(Page {
                offset,
                count: end - offset,
                total: items.len(),
                next_offset: (end < items.len()).then_some(end),
            }),
        )
    } else {
        (BorrowedData::One(data), None)
    };
    let available = limits
        .nodes
        .checked_sub(meta.as_ref().map_or(0, |m| m.nodes()))
        .ok_or(CodecError::Work)?;
    let nodes = if !selection.shape_only || typed {
        check_shape(Some(shape), available, 0)?
    } else {
        available
    };
    if selection.shape_only {
        if matches!(data, Data::Iter(_)) {
            return Err(CodecError::Export(
                "Materialize the selected value before exporting it.".into(),
            ));
        }
        #[derive(Serialize)]
        struct Summary<'a> {
            #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
            shape: Option<ShapeJson<'a>>,
            #[serde(skip_serializing_if = "Option::is_none")]
            meta: Option<wes_core::contracts::metadata::ValueMetadata>,
            kind: &'static str,
            #[serde(skip_serializing_if = "Option::is_none")]
            length: Option<usize>,
            #[serde(skip_serializing_if = "Option::is_none")]
            records: Option<String>,
        }
        let (kind, length) = match data {
            Data::List(items) => ("list", Some(items.len())),
            Data::Record(items) => ("record", Some(items.len())),
            Data::Text(_) => ("text", None),
            Data::Int(_) => ("int", None),
            Data::Decimal(_) => ("decimal", None),
            Data::Bool(_) => ("bool", None),
            Data::Bytes(_) => ("bytes", None),
            Data::Instant(_) => ("instant", None),
            Data::Duration(_) => ("duration", None),
            Data::Interval(_) => ("interval", None),
            Data::Option(_) => ("option", None),
            Data::Iter(_) => unreachable!(),
            Data::Dataset(_) => ("dataset", None),
        };
        return write(
            &Summary {
                shape: typed.then_some(ShapeJson(shape)),
                meta: meta.as_ref().and_then(|m| m.wire()),
                kind,
                length,
                records: match data {
                    Data::Dataset(reference) => Some(reference.records().to_string()),
                    _ => None,
                },
            },
            limits,
        );
    }
    let mut remaining = nodes;
    let mut check_data = |data: &Data| -> Result<(), CodecError> {
        if !data.is_storable_snapshot() {
            return Err(CodecError::Export(
                "Materialize the selected value before exporting it.".into(),
            ));
        }
        remaining = check(
            None,
            data,
            Limits {
                nodes: remaining,
                ..limits
            },
        )?;
        Ok(())
    };
    match body {
        BorrowedData::One(data) => check_data(data)?,
        BorrowedData::List(items) => {
            for data in items {
                check_data(data)?;
            }
        }
    }
    if typed {
        remaining
            .checked_sub(value.provenance().facts().len())
            .and_then(|n| n.checked_sub(value.provenance().cautions().len()))
            .ok_or(CodecError::Work)?;
    }
    struct Projected<'a> {
        body: BorrowedData<'a>,
        shape: &'a Shape,
        value: &'a Value,
        typed: bool,
        meta: Option<&'a wes_core::contracts::metadata::ValueMetadata>,
        page: Option<Page>,
    }
    impl Serialize for Projected<'_> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut object = s.serialize_map(None)?;
            if self.typed {
                object.serialize_entry(
                    "value",
                    &Stored {
                        shape: self.shape,
                        data: self.body,
                        provenance: self.value.provenance(),
                        meta: self.meta,
                    },
                )?;
            } else {
                object.serialize_entry("value", &BorrowedJson(self.body, false))?;
            }
            if let Some(page) = &self.page {
                object.serialize_entry("page", page)?;
            }
            object.end()
        }
    }
    write(
        &Projected {
            body,
            shape,
            value,
            typed,
            meta: meta.as_ref(),
            page,
        },
        limits,
    )
}

#[derive(Serialize)]
struct Page {
    offset: usize,
    count: usize,
    total: usize,
    next_offset: Option<usize>,
}

/// Borrow a bounded structural selection. Callers must enforce the root's egress policy first.
pub fn select<'a>(
    value: &'a Value,
    pointer: &str,
) -> Result<(&'a Data, &'a Shape, String), CodecError> {
    if pointer.len() > 2048 {
        return Err(invalid("select exceeds 2048 bytes."));
    }
    let mut data = value.data();
    let mut shape = value.shape();
    let mut path = String::new();
    if pointer.is_empty() {
        return Ok((data, shape, path));
    }
    let tail = pointer.strip_prefix('/').ok_or_else(|| {
        invalid("select must be a JSON Pointer, e.g. /body/items/0; empty selects the root.")
    })?;
    for (depth, raw) in tail.split('/').enumerate() {
        if depth >= 32 {
            return Err(invalid("select exceeds 32 path segments."));
        }
        let mut segment = String::new();
        let mut chars = raw.chars();
        while let Some(ch) = chars.next() {
            segment.push(if ch == '~' {
                match chars.next() {
                    Some('0') => '~',
                    Some('1') => '/',
                    _ => return Err(invalid("Invalid JSON Pointer escape; use ~0 or ~1.")),
                }
            } else {
                ch
            });
        }
        match data {
            Data::Record(fields) => {
                path.push_str(&wes_core::contracts::metadata::field_segment(&segment));
                data = fields.get(&segment).ok_or_else(|| {
                    invalid(&format!(
                        "No field at select segment {} ({segment:?}).",
                        depth + 1
                    ))
                })?;
                shape = match shape {
                    Shape::Record(record) => record.field(&segment).unwrap_or(&Shape::Unknown),
                    _ => &Shape::Unknown,
                };
            }
            Data::List(items) => {
                path.push_str("/e");
                if segment.is_empty()
                    || !segment.bytes().all(|b| b.is_ascii_digit())
                    || (segment.len() > 1 && segment.starts_with('0'))
                {
                    return Err(invalid(
                        "List path segments must be canonical non-negative indices.",
                    ));
                }
                let index = segment
                    .parse::<usize>()
                    .map_err(|_| invalid("List index is out of range."))?;
                data = items
                    .get(index)
                    .ok_or_else(|| invalid("List index is out of range."))?;
                shape = match shape {
                    Shape::List(element) => element,
                    _ => &Shape::Unknown,
                };
            }
            _ => {
                return Err(invalid(
                    "select traverses a scalar, Option or lazy value. Use calc to unwrap/transform it first.",
                ));
            }
        }
    }
    Ok((data, shape, path))
}
