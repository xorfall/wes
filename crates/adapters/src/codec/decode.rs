use super::{
    CodecError, Limits,
    raw::{self, Context},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::value::RawValue;
use std::collections::{BTreeMap, BTreeSet};
use wes_core::{Data, Primitive, Provenance, RecordShape, Shape, Value};

#[derive(Clone, Debug)]
pub struct DecodedValue {
    pub value: Value,
}

pub fn decode_value(bytes: &[u8], limits: Limits) -> Result<DecodedValue, CodecError> {
    let mut context = Context::new(limits);
    let root = context.root(bytes)?;
    read_value_node(root, &mut context, 0)
}
fn read_value_node(
    root: &RawValue,
    context: &mut Context,
    depth: usize,
) -> Result<DecodedValue, CodecError> {
    // The envelope is a format header, not a domain node. Shape, data and provenance
    // are charged below, including those in an Iter's owned source value.
    let options = context.options;
    let retained = context.retained;
    let object = context.object(root)?;
    if raw::string(raw::required(&object, "format")?)? != super::VALUE_FORMAT
        || raw::scalar::<u32>(raw::required(&object, "version")?)? != super::VALUE_VERSION
    {
        return Err(invalid("unsupported retained-value format or version"));
    }
    context.options = true;
    context.retained = true;
    let shape = read_shape(raw::required(&object, "type")?, context, depth)?;
    let data = read_tagged(raw::required(&object, "data")?, context, depth)?;
    let mut provenance = read_provenance(raw::required(&object, "provenance")?, context)?;
    let fields = context.object(raw::required(&object, "policy")?)?;
    if fields
        .keys()
        .any(|key| !matches!(key.as_str(), "origins" | "unknown" | "dataset_reads"))
    {
        return Err(invalid("invalid value policy"));
    }
    let origins = context.sequence(raw::required(&fields, "origins")?)?;
    if origins.len() > 128 {
        return Err(CodecError::Work);
    }
    let mut policy = wes_core::flow::FlowPolicy::default();
    if let Some(reads) = fields.get("dataset_reads") {
        let reads = context.sequence(reads)?;
        if reads.len() > 128 {
            return Err(CodecError::Work);
        }
        for read in reads {
            let read = context.object(read)?;
            if read.len() != 2 {
                return Err(invalid("invalid dataset read origin"));
            }
            let wire = serde_json::json!({"store": raw::string(raw::required(&read,"store")?)?, "dataset": raw::string(raw::required(&read,"dataset")?)?});
            let origin = serde_json::from_value::<wes_core::flow::DatasetReadOrigin>(wire)
                .map_err(|_| invalid("invalid dataset read origin"))?;
            policy = policy.with_dataset_read(origin);
        }
    }
    for origin in origins {
        policy = policy.from_origin(raw::string(origin)?);
    }
    if raw::scalar::<bool>(raw::required(&fields, "unknown")?)? {
        policy = policy.unknown();
    }
    provenance = provenance.with_policy(&policy);
    context.options = options;
    context.retained = retained;
    let wire = object
        .get("meta")
        .map(|raw| read_metadata(raw, context))
        .transpose()?;
    if let Some(meta) = &wire {
        meta.validate_wire().map_err(invalid)?;
    }
    let meta = match object.get("metaProjection") {
        Some(raw) => {
            let captured = read_metadata(raw, context)?;
            if !captured.needs_projection_snapshot() || captured.wire() != wire {
                return Err(invalid("inconsistent retained metadata projection"));
            }
            Some(captured)
        }
        None => wire,
    };
    Ok(DecodedValue {
        value: Value::new(shape, data, provenance)?.with_metadata(meta),
    })
}
fn invalid(message: &str) -> CodecError {
    CodecError::Invalid(message.into())
}
fn read_shape(raw: &RawValue, context: &mut Context, depth: usize) -> Result<Shape, CodecError> {
    context.visit(depth)?;
    let object = context.object(raw)?;
    Ok(
        match raw::string(raw::required(&object, "kind")?)?.as_str() {
            "meta" => match raw::string(raw::required(&object, "name")?)?.as_str() {
                "ImportPlan" => Shape::Meta(wes_core::MetaType::ImportPlan),
                "DatasetDeletePlan" => Shape::Meta(wes_core::MetaType::DatasetDeletePlan),
                "WorkspaceDeletePlan" => Shape::Meta(wes_core::MetaType::WorkspaceDeletePlan),
                "ViewInstance" => Shape::Meta(wes_core::MetaType::ViewInstance),
                _ => return Err(invalid("unknown management type")),
            },
            "unknown" => Shape::Unknown,
            "primitive" => Shape::Primitive(
                match raw::string(raw::required(&object, "name")?)?.as_str() {
                    "TEXT" => Primitive::Text,
                    "INT" => Primitive::Int,
                    "DECIMAL" => Primitive::Decimal,
                    "BOOL" => Primitive::Bool,
                    "INSTANT" => Primitive::Instant,
                    "DURATION" => Primitive::Duration,
                    "INTERVAL" => Primitive::Interval,
                    "BYTES" => Primitive::Bytes,
                    _ => return Err(invalid("unknown primitive type")),
                },
            ),
            "iter" if context.retained => Shape::Iter(Box::new(read_shape(
                raw::required(&object, "element")?,
                context,
                depth + 1,
            )?)),
            "dataset" if context.retained => Shape::Dataset(Box::new(read_shape(
                raw::required(&object, "element")?,
                context,
                depth + 1,
            )?)),
            "option" if context.options => Shape::Option(Box::new(read_shape(
                raw::required(&object, "element")?,
                context,
                depth + 1,
            )?)),
            "list" => Shape::List(Box::new(read_shape(
                raw::required(&object, "element")?,
                context,
                depth + 1,
            )?)),
            "record" => {
                let name = raw::string(raw::required(&object, "name")?)?;
                let mut fields = vec![];
                for field in context.sequence(raw::required(&object, "fields")?)? {
                    let field = context.object(field)?;
                    fields.push((
                        raw::string(raw::required(&field, "name")?)?,
                        read_shape(raw::required(&field, "type")?, context, depth + 1)?,
                    ));
                }
                Shape::Record(RecordShape::new(name, fields)?)
            }
            _ => return Err(invalid("unknown shape kind")),
        },
    )
}
fn read_provenance(raw: &RawValue, context: &mut Context) -> Result<Provenance, CodecError> {
    let object = context.object(raw)?;
    let mut facts = BTreeMap::new();
    let mut cautions = BTreeSet::new();
    for (key, value) in context.object(raw::required(&object, "facts")?)? {
        context.visit(0)?;
        facts.insert(key, raw::string(value)?);
    }
    for caution in context.sequence(raw::required(&object, "cautions")?)? {
        context.visit(0)?;
        cautions.insert(raw::string(caution)?);
    }
    Ok(Provenance::new(facts, cautions))
}
fn read_tagged(raw: &RawValue, context: &mut Context, depth: usize) -> Result<Data, CodecError> {
    context.visit(depth)?;
    let object = context.object(raw)?;
    let payload = raw::required(&object, "value")?;
    Ok(
        match raw::string(raw::required(&object, "kind")?)?.as_str() {
            "iter" if context.retained => read_iter(payload, context, depth)?,
            "option" if context.options => Data::Option(if payload.get() == "null" {
                None
            } else {
                Some(Box::new(read_tagged(payload, context, depth + 1)?))
            }),
            "text" => Data::Text(raw::string(payload)?.into()),
            "int" => Data::Int(
                raw::string(payload)?
                    .parse()
                    .map_err(|_| invalid("invalid integer"))?,
            ),
            "decimal" => Data::Decimal(raw::string(payload)?.parse()?),
            "bool" => Data::Bool(raw::scalar(payload)?),
            "instant" => Data::Instant(raw::string(payload)?.parse()?),
            "duration" => Data::Duration(raw::string(payload)?.parse()?),
            "interval" => Data::Interval(raw::string(payload)?.parse()?),
            "bytes" => Data::Bytes(
                STANDARD
                    .decode(raw::string(payload)?)
                    .map_err(|_| invalid("invalid canonical base64"))?
                    .into(),
            ),
            "list" => Data::List(
                context
                    .sequence(payload)?
                    .into_iter()
                    .map(|raw| read_tagged(raw, context, depth + 1))
                    .collect::<Result<_, _>>()?,
            ),
            "record" => Data::Record(
                context
                    .object(payload)?
                    .into_iter()
                    .map(|(key, raw)| Ok((key, read_tagged(raw, context, depth + 1)?)))
                    .collect::<Result<_, CodecError>>()?,
            ),
            "dataset" if context.retained => read_dataset(payload, context, depth)?,
            _ => return Err(invalid("unknown data kind")),
        },
    )
}
fn read_dataset(
    payload: &RawValue,
    context: &mut Context,
    depth: usize,
) -> Result<Data, CodecError> {
    let fields = context.object(payload)?;
    if fields.len() != 2 || raw::string(raw::required(&fields, "kind")?)? != "dataset" {
        return Err(invalid("invalid dataset envelope"));
    }
    let reference = context.object(raw::required(&fields, "reference")?)?;
    for _ in &reference {
        context.visit(depth + 1)?;
    }
    let reference = serde_json::from_str(raw::required(&fields, "reference")?.get())
        .map_err(|_| invalid("invalid dataset reference"))?;
    Ok(Data::Dataset(std::sync::Arc::new(reference)))
}
fn read_iter(raw: &RawValue, context: &mut Context, depth: usize) -> Result<Data, CodecError> {
    use wes_core::{ContractCapture, IterMode, IterStage, IterValue};
    let object = context.object(raw)?;
    if raw::scalar::<u32>(raw::required(&object, "version")?)? != 1 {
        return Err(invalid("unsupported Iter recipe version"));
    }
    let mode = IterMode::parse(&raw::string(raw::required(&object, "mode")?)?)
        .ok_or_else(|| invalid("unknown Iter mode"))?;
    let arg = raw::required(&object, "argument")?;
    let argument = if arg.get() == "null" {
        None
    } else {
        Some(raw::string(arg)?)
    };
    let source = read_value_node(raw::required(&object, "source")?, context, depth + 1)?.value;
    let raw_stages = context.sequence(raw::required(&object, "stages")?)?;
    if raw_stages.len() > 64 {
        return Err(invalid("too many Iter stages"));
    }
    let mut stages = vec![];
    for raw in raw_stages {
        context.visit(depth + 1)?;
        let stage = context.object(raw)?;
        let kind = raw::string(raw::required(&stage, "kind")?)?;
        stages.push(match kind.as_str() {
            "take" => IterStage::Take(raw::scalar(raw::required(&stage, "count")?)?),
            "skip" => IterStage::Skip(raw::scalar(raw::required(&stage, "count")?)?),
            "field" => IterStage::Field(raw::string(raw::required(&stage, "name")?)?),
            "check" => {
                let mut packages = vec![];
                let mut bytes = 0usize;
                for raw in context.sequence(raw::required(&stage, "packages")?)? {
                    context.visit(depth + 2)?;
                    let text = raw::string(raw)?;
                    bytes = bytes.saturating_add(text.len());
                    if bytes > 1024 * 1024 || packages.len() >= 1000 {
                        return Err(invalid("captured Iter packages exceed limits"));
                    }
                    packages.push(text);
                }
                IterStage::Check(ContractCapture {
                    name: raw::string(raw::required(&stage, "name")?)?,
                    packages,
                })
            }
            _ => return Err(invalid("unknown Iter stage")),
        });
    }
    let iter = IterValue::new(source, mode, argument, stages)
        .map_err(|error| CodecError::Invalid(error.to_string()))?;
    let expected = read_shape(raw::required(&object, "itemType")?, context, depth + 1)?;
    if &expected != iter.item_shape() {
        return Err(invalid("Iter item type does not match captured recipe"));
    }
    Ok(Data::Iter(std::sync::Arc::new(iter)))
}

fn read_metadata(
    raw: &RawValue,
    context: &mut Context,
) -> Result<wes_core::contracts::metadata::ValueMetadata, CodecError> {
    use wes_core::contracts::metadata::{MAX_BYTES, ValueMetadata};
    if raw.get().len() > MAX_BYTES {
        return Err(invalid("oversized value metadata"));
    }
    // Refuse duplicate keys before serde's map decoding; charge all metadata nodes.
    fn walk(raw: &RawValue, context: &mut Context, depth: usize) -> Result<(), CodecError> {
        context.visit(depth)?;
        match raw.get().as_bytes().first() {
            Some(b'{') => {
                for (_, v) in context.object(raw)? {
                    walk(v, context, depth + 1)?;
                }
            }
            Some(b'[') => {
                for v in context.sequence(raw)? {
                    walk(v, context, depth + 1)?;
                }
            }
            Some(b'n') => {
                return Err(invalid(
                    "null is not valid captured metadata; omit unknown properties",
                ));
            }
            _ => {}
        }
        Ok(())
    }
    walk(raw, context, 0)?;
    let meta: ValueMetadata = serde_json::from_str(raw.get())?;
    meta.validate().map_err(invalid)?;
    Ok(meta)
}
