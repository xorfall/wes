use super::{CodecError, Limits};
use base64::{display::Base64Display, engine::general_purpose::STANDARD};
use indexmap::IndexMap;
use serde::{
    Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq, SerializeStruct},
};
use std::{
    fmt,
    io::{self, Write},
};
use wes_core::{Data, Primitive, Shape, Value};

pub fn encode_value(value: &Value, limits: Limits) -> Result<Vec<u8>, CodecError> {
    if value.provenance().policy().is_confidential() {
        return Err(CodecError::Invalid(
            "private values cannot be encoded for retention".into(),
        ));
    }
    encode_protected_value(value, limits)
}
/// Operation-local encoding for authenticated storage, never a plaintext publication API.
pub(crate) fn encode_protected_value(value: &Value, limits: Limits) -> Result<Vec<u8>, CodecError> {
    if value.provenance().policy().is_private() {
        return Err(CodecError::Invalid(
            "memory-only values cannot be persisted".into(),
        ));
    }
    let nodes = limits
        .nodes
        .checked_sub(value.metadata().map_or(0, |m| m.nodes()))
        .ok_or(CodecError::Work)?
        .checked_sub(value.provenance().facts().len())
        .and_then(|left| left.checked_sub(value.provenance().cautions().len()))
        .ok_or(CodecError::Work)?;
    check(
        Some(value.shape()),
        value.data(),
        Limits { nodes, ..limits },
    )?;
    write(&Stored::from(value), limits)
}
/// Browser data-plane envelope. Retained files have their own versioned representation.
pub fn encode_display_value(value: &Value, limits: Limits) -> Result<Vec<u8>, CodecError> {
    let nodes = limits
        .nodes
        .checked_sub(value.metadata().map_or(0, |m| m.nodes()))
        .ok_or(CodecError::Work)?
        .checked_sub(value.provenance().facts().len())
        .ok_or(CodecError::Work)?;
    check(
        Some(value.shape()),
        value.data(),
        Limits { nodes, ..limits },
    )?;
    struct DisplayValue<'a>(&'a Value);
    impl Serialize for DisplayValue<'_> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut object = s.serialize_struct("StoredValue", 3)?;
            object.serialize_field("type", &ShapeJson(self.0.shape()))?;
            object.serialize_field("provenance", self.0.provenance().facts())?;
            object.serialize_field("data", &DataJson(self.0.data(), JsonDataMode::Value))?;
            if let Some(meta) = self.0.metadata() {
                meta.validate().map_err(serde::ser::Error::custom)?;
                if let Some(wire) = meta.wire() {
                    object.serialize_field("meta", &wire)?;
                }
            }
            object.end()
        }
    }
    write(&DisplayValue(value), limits)
}
/// Display/transport JSON, not a retained-value envelope. Bytes remain explicitly base64 here;
/// content-aware UI rendering is a separate boundary and must not reinterpret every string.
pub fn encode_json(data: &Data, limits: Limits) -> Result<Vec<u8>, CodecError> {
    check(None, data, limits)?;
    write(&DataJson(data, JsonDataMode::Value), limits)
}
/// Human-readable display JSON with the same exact numbers, depth and output budgets.
pub fn encode_json_pretty(data: &Data, limits: Limits) -> Result<Vec<u8>, CodecError> {
    check(None, data, limits)?;
    write_with(&DataJson(data, JsonDataMode::Value), limits, true)
}
/// Human JSON normalizes only Decimal presentation; stored/transport values remain unchanged.
pub fn encode_json_human(data: &Data, limits: Limits, pretty: bool) -> Result<Vec<u8>, CodecError> {
    check(None, data, limits)?;
    write_with(&DataJson(data, JsonDataMode::Human), limits, pretty)
}
/// Foreign request data: native Option becomes JSON null/value, not the display envelope.
pub fn encode_request_data(data: &Data, limits: Limits) -> Result<Vec<u8>, CodecError> {
    check(None, data, limits)?;
    write(&RequestJson(data, limits.bytes), limits)
}
pub(crate) struct ArgumentError<'a> {
    pub name: Option<&'a str>,
    pub error: CodecError,
}
pub(crate) fn check_arguments(
    arguments: &IndexMap<String, Value>,
    limits: Limits,
) -> Result<(), CodecError> {
    check_arguments_detailed(arguments, limits).map_err(|e| e.error)
}
/// Same aggregate work/size checks, retaining which argument exhausted the boundary.
pub(crate) fn check_arguments_detailed(
    arguments: &IndexMap<String, Value>,
    limits: Limits,
) -> Result<(), ArgumentError<'_>> {
    let mut remaining = limits.nodes.checked_sub(1).ok_or(ArgumentError {
        name: None,
        error: CodecError::Work,
    })?;
    for (name, value) in arguments {
        let fail = |error| ArgumentError {
            name: Some(name.as_str()),
            error,
        };
        if !value.data().is_inline() || !value.shape().is_inline() {
            return Err(fail(CodecError::Invalid(
                "provider arguments require materialized data".into(),
            )));
        }
        if name.len() > limits.bytes {
            return Err(fail(CodecError::Bytes));
        }
        remaining = check(
            None,
            value.data(),
            Limits {
                nodes: remaining,
                ..limits
            },
        )
        .map_err(fail)?;
    }
    Ok(())
}

struct RequestJson<'a>(&'a Data, usize);
impl Serialize for RequestJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Data::Iter(_) | Data::Dataset(_) => Err(serde::ser::Error::custom(
                "Iter and Dataset cannot cross a provider boundary; select a finite page first",
            )),
            Data::Option(None) => s.serialize_none(),
            Data::Option(Some(value)) => RequestJson(value, self.1).serialize(s),
            Data::Decimal(n) => {
                let text = n.plain_text(self.1).ok_or_else(|| {
                    serde::ser::Error::custom("request decimal exceeds byte budget")
                })?;
                let raw = serde_json::value::RawValue::from_string(text)
                    .map_err(serde::ser::Error::custom)?;
                raw.serialize(s)
            }
            Data::List(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(&RequestJson(item, self.1))?;
                }
                seq.end()
            }
            Data::Record(fields) => {
                let mut entries = fields.iter().collect::<Vec<_>>();
                entries.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
                let mut map = s.serialize_map(Some(entries.len()))?;
                for (key, data) in entries {
                    map.serialize_entry(key, &RequestJson(data, self.1))?;
                }
                map.end()
            }
            _ => Payload(self.0, JsonDataMode::Value).serialize(s),
        }
    }
}

fn check(shape: Option<&Shape>, data: &Data, limits: Limits) -> Result<usize, CodecError> {
    check_at(shape, data, limits, 0)
}
fn check_shape(
    shape: Option<&Shape>,
    mut remaining: usize,
    depth: usize,
) -> Result<usize, CodecError> {
    let mut shapes = shape.into_iter().map(|s| (s, depth)).collect::<Vec<_>>();
    while let Some((shape, depth)) = shapes.pop() {
        if depth > 200 {
            return Err(CodecError::Depth);
        }
        remaining = remaining.checked_sub(1).ok_or(CodecError::Work)?;
        match shape {
            Shape::List(element)
            | Shape::Option(element)
            | Shape::Iter(element)
            | Shape::Dataset(element) => shapes.push((element, depth + 1)),
            Shape::Record(record) => {
                if record.fields().len() > remaining.saturating_sub(shapes.len()) {
                    return Err(CodecError::Work);
                }
                shapes.extend(record.fields().map(|(_, shape)| (shape, depth + 1)))
            }
            _ => {}
        }
    }
    Ok(remaining)
}
fn check_at(
    shape: Option<&Shape>,
    data: &Data,
    limits: Limits,
    depth: usize,
) -> Result<usize, CodecError> {
    let mut remaining = check_shape(shape, limits.nodes, depth)?;
    let mut pending = vec![(data, depth)];
    while let Some((data, depth)) = pending.pop() {
        if depth > 200 {
            return Err(CodecError::Depth);
        }
        remaining = remaining.checked_sub(1).ok_or(CodecError::Work)?;
        match data {
            Data::Iter(iter) => {
                remaining = remaining
                    .checked_sub(iter.source().metadata().map_or(0, |m| m.nodes()))
                    .and_then(|left| left.checked_sub(iter.source().provenance().facts().len()))
                    .and_then(|left| left.checked_sub(iter.source().provenance().cautions().len()))
                    .ok_or(CodecError::Work)?;
                remaining = check_at(
                    Some(iter.source().shape()),
                    iter.source().data(),
                    Limits {
                        nodes: remaining,
                        ..limits
                    },
                    depth + 1,
                )?;
                remaining = check_shape(Some(iter.item_shape()), remaining, depth + 1)?;
                for stage in iter.stages() {
                    if depth + 1 > 200 {
                        return Err(CodecError::Depth);
                    }
                    remaining = remaining.checked_sub(1).ok_or(CodecError::Work)?;
                    if let wes_core::IterStage::Check(capture) = stage {
                        if !capture.packages.is_empty() && depth + 2 > 200 {
                            return Err(CodecError::Depth);
                        }
                        remaining = remaining
                            .checked_sub(capture.packages.len())
                            .ok_or(CodecError::Work)?;
                    }
                }
            }
            Data::Option(Some(item)) => pending.push((item, depth + 1)),
            Data::Text(text) if text.len() > limits.bytes => return Err(CodecError::Bytes),
            Data::Bytes(bytes) if bytes.len() > limits.bytes => return Err(CodecError::Bytes),
            Data::List(items) => {
                if items.len() > remaining.saturating_sub(pending.len()) {
                    return Err(CodecError::Work);
                }
                pending.extend(items.iter().map(|item| (item, depth + 1)));
            }
            Data::Record(fields) => {
                if fields.len() > remaining.saturating_sub(pending.len()) {
                    return Err(CodecError::Work);
                }
                pending.extend(fields.values().map(|item| (item, depth + 1)));
            }
            _ => {}
        }
    }
    Ok(remaining)
}
struct Limited {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("JSON output budget exceeded"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) fn write(value: &impl Serialize, limits: Limits) -> Result<Vec<u8>, CodecError> {
    write_with(value, limits, false)
}
fn write_with(value: &impl Serialize, limits: Limits, pretty: bool) -> Result<Vec<u8>, CodecError> {
    let mut writer = Limited {
        bytes: vec![],
        limit: limits.bytes,
        exceeded: false,
    };
    let result = if pretty {
        serde_json::to_writer_pretty(&mut writer, value)
    } else {
        serde_json::to_writer(&mut writer, value)
    };
    if writer.exceeded {
        return Err(CodecError::Bytes);
    }
    result?;
    Ok(writer.bytes)
}
struct Displayed<'a, T: ?Sized>(&'a T);
impl<T: fmt::Display + ?Sized> Serialize for Displayed<'_, T> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self.0)
    }
}
mod projection;
pub use projection::{ValueSelection, encode_selection, select as select_value};

struct Stored<'a> {
    shape: &'a Shape,
    data: BorrowedData<'a>,
    provenance: &'a wes_core::Provenance,
    meta: Option<&'a wes_core::contracts::metadata::ValueMetadata>,
}
impl<'a> From<&'a Value> for Stored<'a> {
    fn from(value: &'a Value) -> Self {
        Self {
            shape: value.shape(),
            data: BorrowedData::One(value.data()),
            provenance: value.provenance(),
            meta: value.metadata(),
        }
    }
}
#[derive(Clone, Copy)]
enum BorrowedData<'a> {
    One(&'a Data),
    List(&'a [Data]),
}
struct BorrowedJson<'a>(BorrowedData<'a>, bool);
impl Serialize for BorrowedJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            BorrowedData::One(data) => DataJson(
                data,
                if self.1 {
                    JsonDataMode::Stored
                } else {
                    JsonDataMode::Value
                },
            )
            .serialize(s),
            BorrowedData::List(items) => {
                struct Items<'a>(&'a [Data], bool);
                impl Serialize for Items<'_> {
                    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                        let mut seq = s.serialize_seq(Some(self.0.len()))?;
                        for data in self.0 {
                            seq.serialize_element(&DataJson(
                                data,
                                if self.1 {
                                    JsonDataMode::Stored
                                } else {
                                    JsonDataMode::Value
                                },
                            ))?;
                        }
                        seq.end()
                    }
                }
                if self.1 {
                    let mut map = s.serialize_map(Some(2))?;
                    map.serialize_entry("kind", "list")?;
                    map.serialize_entry("value", &Items(items, true))?;
                    map.end()
                } else {
                    Items(items, false).serialize(s)
                }
            }
        }
    }
}
impl Serialize for Stored<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.provenance.policy().is_private() {
            return Err(serde::ser::Error::custom(
                "private values cannot be retained",
            ));
        }
        let mut object = s.serialize_struct("StoredValue", 5)?;
        object.serialize_field("format", super::VALUE_FORMAT)?;
        object.serialize_field("version", &super::VALUE_VERSION)?;
        {
            #[derive(Serialize)]
            struct Policy<'a> {
                origins: &'a std::collections::BTreeSet<String>,
                unknown: bool,
                #[serde(skip_serializing_if = "Option::is_none")]
                confidential: Option<&'static str>,
                #[serde(skip_serializing_if = "std::collections::BTreeSet::is_empty")]
                dataset_reads: &'a std::collections::BTreeSet<wes_core::flow::DatasetReadOrigin>,
            }
            let policy = self.provenance.policy();
            object.serialize_field(
                "policy",
                &Policy {
                    origins: policy.origins(),
                    unknown: policy.is_unknown(),
                    confidential: policy
                        .is_confidential()
                        .then_some(match policy.residence() {
                            wes_core::flow::Residence::Temporary => "temporary",
                            wes_core::flow::Residence::Retainable => "retainable",
                            wes_core::flow::Residence::Memory => {
                                return Err(serde::ser::Error::custom("memory-only value"));
                            }
                        }),
                    dataset_reads: policy.dataset_reads(),
                },
            )?;
        }
        object.serialize_field("type", &ShapeJson(self.shape))?;
        #[derive(Serialize)]
        struct Origin<'a> {
            facts: &'a std::collections::BTreeMap<String, String>,
            cautions: &'a std::collections::BTreeSet<String>,
        }
        object.serialize_field(
            "provenance",
            &Origin {
                facts: self.provenance.facts(),
                cautions: self.provenance.cautions(),
            },
        )?;
        object.serialize_field("data", &BorrowedJson(self.data, true))?;
        if let Some(meta) = self.meta {
            meta.validate().map_err(serde::ser::Error::custom)?;
            if let Some(wire) = meta.wire() {
                object.serialize_field("meta", &wire)?;
            }
            if meta.needs_projection_snapshot() {
                object.serialize_field("metaProjection", meta)?;
            }
        }
        object.end()
    }
}
struct ShapeJson<'a>(&'a Shape);
impl Serialize for ShapeJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(None)?;
        match self.0 {
            Shape::Meta(t) => {
                map.serialize_entry("kind", "meta")?;
                map.serialize_entry("name", &t.to_string())?;
            }
            Shape::Unknown => map.serialize_entry("kind", "unknown")?,
            Shape::Primitive(primitive) => {
                map.serialize_entry("kind", "primitive")?;
                map.serialize_entry(
                    "name",
                    match primitive {
                        Primitive::Text => "TEXT",
                        Primitive::Int => "INT",
                        Primitive::Decimal => "DECIMAL",
                        Primitive::Bool => "BOOL",
                        Primitive::Instant => "INSTANT",
                        Primitive::Duration => "DURATION",
                        Primitive::Interval => "INTERVAL",
                        Primitive::Bytes => "BYTES",
                    },
                )?;
            }
            Shape::Iter(element) => {
                map.serialize_entry("kind", "iter")?;
                map.serialize_entry("element", &ShapeJson(element))?;
            }
            Shape::Dataset(element) => {
                map.serialize_entry("kind", "dataset")?;
                map.serialize_entry("element", &ShapeJson(element))?;
            }
            Shape::Option(element) => {
                map.serialize_entry("kind", "option")?;
                map.serialize_entry("element", &ShapeJson(element))?;
            }
            Shape::List(element) => {
                map.serialize_entry("kind", "list")?;
                map.serialize_entry("element", &ShapeJson(element))?;
            }
            Shape::Record(record) => {
                map.serialize_entry("kind", "record")?;
                map.serialize_entry("name", record.name())?;
                struct Fields<'a>(&'a wes_core::RecordShape);
                impl Serialize for Fields<'_> {
                    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                        let mut seq = s.serialize_seq(Some(self.0.fields().len()))?;
                        #[derive(Serialize)]
                        struct Field<'a> {
                            name: &'a str,
                            #[serde(rename = "type")]
                            shape: ShapeJson<'a>,
                        }
                        for (name, shape) in self.0.fields() {
                            seq.serialize_element(&Field {
                                name,
                                shape: ShapeJson(shape),
                            })?;
                        }
                        seq.end()
                    }
                }
                map.serialize_entry("fields", &Fields(record))?;
            }
        }
        map.end()
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum JsonDataMode {
    Value,
    Stored,
    Human,
}
struct DataJson<'a>(&'a Data, JsonDataMode);
impl Serialize for DataJson<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.1 == JsonDataMode::Stored {
            let mut map = s.serialize_struct("Data", 2)?;
            map.serialize_field(
                "kind",
                match self.0 {
                    Data::Iter(_) => "iter",
                    Data::Dataset(_) => "dataset",
                    Data::Option(_) => "option",
                    Data::Text(_) => "text",
                    Data::Int(_) => "int",
                    Data::Decimal(_) => "decimal",
                    Data::Bool(_) => "bool",
                    Data::Instant(_) => "instant",
                    Data::Duration(_) => "duration",
                    Data::Interval(_) => "interval",
                    Data::Bytes(_) => "bytes",
                    Data::List(_) => "list",
                    Data::Record(_) => "record",
                },
            )?;
            map.serialize_field("value", &Payload(self.0, JsonDataMode::Stored))?;
            map.end()
        } else {
            Payload(self.0, self.1).serialize(s)
        }
    }
}
struct Payload<'a>(&'a Data, JsonDataMode);
impl Serialize for Payload<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Data::Dataset(reference) => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("kind", "dataset")?;
                map.serialize_entry("reference", reference.as_ref())?;
                map.end()
            }
            Data::Iter(iter) => {
                let mut map = s.serialize_map(None)?;
                map.serialize_entry("kind", "iter")?;
                map.serialize_entry("mode", iter.mode().name())?;
                map.serialize_entry("itemType", &ShapeJson(iter.item_shape()))?;
                if self.1 == JsonDataMode::Stored {
                    map.serialize_entry("version", &1u32)?;
                    map.serialize_entry("source", &Stored::from(iter.source()))?;
                    map.serialize_entry("argument", &iter.argument())?;
                    map.serialize_entry("stages", &Stages(iter.stages()))?;
                } else {
                    map.serialize_entry(
                        "itemContract",
                        &iter.item_contract().map(|c| c.name().to_owned()),
                    )?;
                    map.serialize_entry("stages", &iter.stages().len())?;
                    map.serialize_entry("sourceType", &ShapeJson(iter.source().shape()))?;
                }
                map.end()
            }

            Data::Option(value) => {
                if self.1 == JsonDataMode::Stored {
                    match value {
                        None => s.serialize_none(),
                        Some(value) => DataJson(value, JsonDataMode::Stored).serialize(s),
                    }
                } else {
                    let mut map = s.serialize_map(None)?;
                    map.serialize_entry("kind", if value.is_some() { "some" } else { "none" })?;
                    if let Some(value) = value {
                        map.serialize_entry("value", &DataJson(value, self.1))?;
                    }
                    map.end()
                }
            }
            Data::Text(text) => s.serialize_str(text),
            Data::Int(number) if self.1 == JsonDataMode::Stored => s.collect_str(number),
            Data::Int(number) => s.serialize_i64(*number),
            Data::Decimal(number) if self.1 == JsonDataMode::Stored => s.collect_str(number),
            Data::Decimal(number) => {
                // Validate a compact exact lexeme with the JSON library, never expand its exponent.
                let raw =
                    serde_json::value::RawValue::from_string(if self.1 == JsonDataMode::Human {
                        number.normalized().to_string()
                    } else {
                        number.to_string()
                    })
                    .map_err(serde::ser::Error::custom)?;
                raw.serialize(s)
            }
            Data::Bool(flag) => s.serialize_bool(*flag),
            Data::Instant(instant) => s.collect_str(instant),
            Data::Duration(duration) => s.collect_str(duration),
            Data::Interval(interval) => s.collect_str(interval),
            Data::Bytes(bytes) => Displayed(&Base64Display::new(bytes, &STANDARD)).serialize(s),
            Data::List(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(&DataJson(item, self.1))?;
                }
                seq.end()
            }
            Data::Record(fields) => {
                let mut map = s.serialize_map(Some(fields.len()))?;
                for (key, value) in fields {
                    map.serialize_entry(key, &DataJson(value, self.1))?;
                }
                map.end()
            }
        }
    }
}

struct Stages<'a>(&'a [wes_core::IterStage]);
impl Serialize for Stages<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        struct Stage<'a>(&'a wes_core::IterStage);
        impl Serialize for Stage<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                use wes_core::IterStage::*;
                let mut map = s.serialize_map(None)?;
                match self.0 {
                    Take(n) | Skip(n) => {
                        map.serialize_entry(
                            "kind",
                            if matches!(self.0, Take(_)) {
                                "take"
                            } else {
                                "skip"
                            },
                        )?;
                        map.serialize_entry("count", n)?;
                    }
                    Field(name) => {
                        map.serialize_entry("kind", "field")?;
                        map.serialize_entry("name", name)?;
                    }
                    Check(c) => {
                        map.serialize_entry("kind", "check")?;
                        map.serialize_entry("name", &c.name)?;
                        map.serialize_entry("packages", &c.packages)?;
                    }
                }
                map.end()
            }
        }
        let mut seq = s.serialize_seq(Some(self.0.len()))?;
        for stage in self.0 {
            seq.serialize_element(&Stage(stage))?;
        }
        seq.end()
    }
}
