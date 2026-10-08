//! serde_json owns all JSON grammar/escaping. RawValue preserves numeric lexemes and lets
//! domain decoding avoid an intermediate tree, floating point, and reserved serde object keys.
use super::{CodecError, Limits};
use indexmap::IndexMap;
use serde::{
    Deserialize, Deserializer,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::value::RawValue;
use std::fmt;
use wes_core::{
    Data, Decimal, Primitive,
    contracts::{Contract, ContractKind},
};

pub(crate) struct Context {
    pub limits: Limits,
    pub options: bool,
    pub retained: bool,
    left: usize,
    work: usize,
    validation_left: usize,
}
impl Context {
    pub fn new(limits: Limits) -> Self {
        Self {
            options: false,
            retained: false,
            left: limits.nodes,
            validation_left: limits.nodes.min(100_000),
            work: limits.bytes.saturating_mul(16),
            limits,
        }
    }
    pub fn root<'a>(&mut self, bytes: &'a [u8]) -> Result<&'a RawValue, CodecError> {
        if bytes.len() > self.limits.bytes {
            return Err(CodecError::Bytes);
        }
        // RawValue's library-owned scanner is iterative even for adversarially deep documents.
        Ok(serde_json::from_slice(
            bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes),
        )?)
    }
    pub fn visit(&mut self, depth: usize) -> Result<(), CodecError> {
        if depth > 200 {
            return Err(CodecError::Depth);
        }
        self.left = self.left.checked_sub(1).ok_or(CodecError::Work)?;
        Ok(())
    }
    fn scan(&mut self, raw: &RawValue) -> Result<(), CodecError> {
        self.work = self
            .work
            .checked_sub(raw.get().len())
            .ok_or(CodecError::Work)?;
        Ok(())
    }
    pub fn object<'a>(
        &mut self,
        raw: &'a RawValue,
    ) -> Result<IndexMap<String, &'a RawValue>, CodecError> {
        self.scan(raw)?;
        // Constant envelope/shape headers are not domain nodes. A small floor permits their
        // fields even when the final scalar consumed the remaining domain-node budget.
        Ok(Object {
            limit: self.left.max(8),
        }
        .deserialize(&mut serde_json::Deserializer::from_str(raw.get()))?)
    }
    pub fn sequence<'a>(&mut self, raw: &'a RawValue) -> Result<Vec<&'a RawValue>, CodecError> {
        self.scan(raw)?;
        Ok(Sequence { limit: self.left }
            .deserialize(&mut serde_json::Deserializer::from_str(raw.get()))?)
    }
}
pub(super) fn string(raw: &RawValue) -> Result<String, CodecError> {
    Ok(serde_json::from_str(raw.get())?)
}
pub(super) fn number(raw: &RawValue) -> Result<Decimal, CodecError> {
    if !matches!(raw.get().as_bytes().first(), Some(b'-' | b'0'..=b'9')) {
        return Err(CodecError::Invalid("expected a number".into()));
    }
    let number: Decimal = raw.get().parse()?;
    // Unversioned untagged JSON input bound. Versioned exact Decimal strings are a different format.
    if number.scale().abs() > 10_000 {
        return Err(CodecError::Invalid(
            "JSON number exceeds supported scale".into(),
        ));
    }
    Ok(number)
}
pub(super) fn required<'a>(
    object: &IndexMap<String, &'a RawValue>,
    key: &str,
) -> Result<&'a RawValue, CodecError> {
    object
        .get(key)
        .copied()
        .ok_or_else(|| CodecError::Invalid(format!("missing '{key}'")))
}

struct Object {
    limit: usize,
}
impl<'de> DeserializeSeed<'de> for Object {
    type Value = IndexMap<String, &'de RawValue>;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}
impl<'de> Visitor<'de> for Object {
    type Value = IndexMap<String, &'de RawValue>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an object with distinct keys")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut fields = IndexMap::new();
        while let Some(key) = map.next_key::<String>()? {
            if fields.len() >= self.limit {
                return Err(de::Error::custom("object exceeds item budget"));
            }
            let value = map.next_value::<&RawValue>()?;
            if fields.insert(key, value).is_some() {
                return Err(de::Error::custom("duplicate object key"));
            }
        }
        Ok(fields)
    }
}
struct Sequence {
    limit: usize,
}
impl<'de> DeserializeSeed<'de> for Sequence {
    type Value = Vec<&'de RawValue>;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for Sequence {
    type Value = Vec<&'de RawValue>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an array within its item budget")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Self::Value, A::Error> {
        let mut items = vec![];
        while let Some(item) = sequence.next_element::<&RawValue>()? {
            if items.len() >= self.limit {
                return Err(de::Error::custom("array exceeds item budget"));
            }
            items.push(item);
        }
        Ok(items)
    }
}

// A borrowed RawValue does not inspect scalar grammar here: deserialization already checked it.
pub(super) fn scalar<T: for<'de> Deserialize<'de>>(raw: &RawValue) -> Result<T, CodecError> {
    Ok(serde_json::from_str(raw.get())?)
}

/// Validate all fields, including ignored/unknown ones, before deserializing a shallow wire DTO.
/// JSONL records have a smaller fixed-schema depth bound than arbitrary user values.
pub(super) fn checked_document(bytes: &[u8], limits: Limits) -> Result<&RawValue, CodecError> {
    let mut context = Context::new(limits);
    let root = context.root(bytes)?;
    let mut pending = vec![(root, 0)];
    while let Some((raw, depth)) = pending.pop() {
        if depth > 32 {
            return Err(CodecError::Depth);
        }
        context.visit(depth)?;
        let children = match raw.get().as_bytes().first() {
            Some(b'{') => context.object(raw)?.into_values().collect::<Vec<_>>(),
            Some(b'[') => context.sequence(raw)?,
            Some(b'"') => {
                string(raw)?;
                vec![]
            }
            _ => vec![],
        };
        if children.len() > context.left.saturating_sub(pending.len()) {
            return Err(CodecError::Work);
        }
        pending.extend(children.into_iter().map(|child| (child, depth + 1)));
    }
    Ok(root)
}

/// JSON for new nullable boundaries. Null remains explicit and object fields are never dropped.
pub(crate) fn present(
    raw: &RawValue,
    context: &mut Context,
    depth: usize,
) -> Result<Data, CodecError> {
    present_for_contract(raw, context, depth, None)
}

/// HTTP response numbers are interpreted from their original JSON tokens. A declared Decimal
/// accepts any JSON number, without converting native Int data or accepting numeric strings.
pub(super) fn present_for_contract(
    raw: &RawValue,
    context: &mut Context,
    depth: usize,
    contract: Option<&Contract>,
) -> Result<Data, CodecError> {
    context.visit(depth)?;
    if let Some(contract) = contract {
        match contract.kind() {
            ContractKind::Scalar(
                kind @ (Primitive::Instant | Primitive::Duration | Primitive::Interval),
            ) if raw.get().starts_with('"') => {
                return wes_core::literals::read(&string(raw)?, &wes_core::Shape::Primitive(*kind))
                    .ok_or(CodecError::Contract);
            }
            ContractKind::Option(element) => {
                return if raw.get() == "null" {
                    Ok(Data::Option(None))
                } else {
                    present_for_contract(raw, context, depth + 1, Some(element))
                        .map(|value| Data::Option(Some(Box::new(value))))
                };
            }
            ContractKind::Union(a, b) => {
                let mut selected = None;
                for alternative in [a, b] {
                    let candidate =
                        match present_for_contract(raw, context, depth + 1, Some(alternative)) {
                            Ok(value) => value,
                            Err(CodecError::Contract) => continue,
                            Err(error) => return Err(error),
                        };
                    let issues = alternative
                        .issues_with_budget(&candidate, &|| false, &mut context.validation_left)
                        .expect("non-cancellable bounded JSON interpretation");
                    if issues.iter().any(|issue| issue.code == "TYP006") {
                        return Err(CodecError::Work);
                    }
                    if !issues.is_empty() {
                        continue;
                    }
                    if selected.as_ref().is_some_and(|value| value != &candidate) {
                        return Err(CodecError::AmbiguousContract);
                    }
                    selected = Some(candidate);
                }
                return selected.ok_or(CodecError::Contract);
            }
            _ => {}
        }
    }
    Ok(match raw.get().as_bytes().first() {
        Some(b'"') => Data::Text(string(raw)?.into()),
        Some(b't' | b'f') => Data::Bool(scalar(raw)?),
        Some(b'n') => Data::Option(None),
        Some(b'[') => Data::List(
            context
                .sequence(raw)?
                .into_iter()
                .map(|v| {
                    present_for_contract(
                        v,
                        context,
                        depth + 1,
                        contract.and_then(Contract::json_element),
                    )
                })
                .collect::<Result<_, _>>()?,
        ),
        Some(b'{') => Data::Record(
            context
                .object(raw)?
                .into_iter()
                .map(|(k, v)| {
                    let value = present_for_contract(
                        v,
                        context,
                        depth + 1,
                        contract.and_then(|c| c.json_field(&k)),
                    )?;
                    Ok((k, value))
                })
                .collect::<Result<_, CodecError>>()?,
        ),
        _ => {
            let n = number(raw)?;
            if contract.and_then(Contract::json_scalar) == Some(Primitive::Decimal) {
                Data::Decimal(n)
            } else if !raw.get().contains(['.', 'e', 'E']) {
                n.exact_i64().map(Data::Int).unwrap_or(Data::Decimal(n))
            } else {
                Data::Decimal(n)
            }
        }
    })
}
