//! Complete, bounded resolved schemas. Presentation previews never stand in for validation.
use super::{Contract, ContractDisplay, ContractField, ContractKind, ContractLimits, EnumTone};
use crate::{Data, Primitive, capability::enum_spelling, literals};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    io::Write,
    sync::Arc,
};
use thiserror::Error;

#[derive(Clone, Copy, Debug)]
pub struct SnapshotLimits {
    pub bytes: usize,
    pub nodes: usize,
    pub depth: usize,
    pub patterns: usize,
}
impl Default for SnapshotLimits {
    fn default() -> Self {
        Self {
            bytes: 4 * 1024 * 1024,
            nodes: 4096,
            depth: 64,
            patterns: 64,
        }
    }
}
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SnapshotError {
    #[error("resolved schema exceeds its {0} limit")]
    Limit(&'static str),
    #[error("invalid resolved schema snapshot")]
    Invalid,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Node {
    digest: String,
    canonical: Json,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    format: String,
    version: u32,
    root: String,
    nodes: Vec<Node>,
}
/// Immutable validated bytes and their resolved root. Construction cannot consult a registry.
#[derive(Clone, Debug)]
pub struct ResolvedContractBundle {
    bytes: Arc<[u8]>,
    root: Arc<Contract>,
}
impl ResolvedContractBundle {
    pub fn capture(root: Arc<Contract>, limits: SnapshotLimits) -> Result<Self, SnapshotError> {
        check_limits(limits)?;
        let mut capture = Capture {
            limits,
            seen: HashSet::new(),
            nodes: Vec::new(),
            charge: 0,
            patterns: 0,
        };
        capture.visit(&root, 0)?;
        let wire = Wire {
            format: "wes.resolved-contract".into(),
            version: 1,
            root: root.digest().into(),
            nodes: capture.nodes,
        };
        let bytes = encode(&wire, limits.bytes)?;
        // The same admission path is used for locally captured and foreign bundles.
        Self::decode(&bytes, limits)
    }
    pub fn decode(bytes: &[u8], limits: SnapshotLimits) -> Result<Self, SnapshotError> {
        check_limits(limits)?;
        if bytes.len() > limits.bytes {
            return Err(SnapshotError::Limit("bytes"));
        }
        let wire: Wire = serde_json::from_slice(bytes).map_err(|_| SnapshotError::Invalid)?;
        if wire.format != "wes.resolved-contract" || wire.version != 1 || wire.nodes.is_empty() {
            return Err(SnapshotError::Invalid);
        }
        if wire.nodes.len() > limits.nodes {
            return Err(SnapshotError::Limit("nodes"));
        }
        let mut remaining = limits.nodes;
        for node in &wire.nodes {
            check_json(&node.canonical, 0, &mut remaining)?;
        }
        // Canonical-only encoding also refuses duplicate object keys and alternate numeric forms.
        if encode(&wire, limits.bytes)? != bytes {
            return Err(SnapshotError::Invalid);
        }
        let mut resolved = BTreeMap::<String, (Arc<Contract>, usize)>::new();
        let mut patterns = 0usize;
        for node in &wire.nodes {
            let (contract, depth) = restore(&node.canonical, &resolved, limits, &mut patterns)?;
            if contract.digest() != node.digest
                || contract.canonical() != node.canonical
                || resolved
                    .insert(node.digest.clone(), (Arc::new(contract), depth))
                    .is_some()
            {
                return Err(SnapshotError::Invalid);
            }
        }
        let root = resolved
            .get(&wire.root)
            .ok_or(SnapshotError::Invalid)?
            .0
            .clone();
        let mut reachable = BTreeSet::new();
        visit_digests(&root, &mut reachable);
        if reachable.len() != resolved.len() {
            return Err(SnapshotError::Invalid);
        }
        Ok(Self {
            bytes: bytes.into(),
            root,
        })
    }
    pub fn root(&self) -> &Arc<Contract> {
        &self.root
    }
    pub fn encoded(&self) -> &[u8] {
        &self.bytes
    }
    pub fn digest(&self) -> &str {
        self.root.digest()
    }
}
fn check_limits(limits: SnapshotLimits) -> Result<(), SnapshotError> {
    if limits.bytes == 0
        || limits.nodes == 0
        || limits.depth == 0
        || limits.depth > 128
        || limits.patterns == 0
    {
        return Err(SnapshotError::Invalid);
    }
    Ok(())
}
fn check_json(value: &Json, depth: usize, remaining: &mut usize) -> Result<(), SnapshotError> {
    *remaining = remaining
        .checked_sub(1)
        .ok_or(SnapshotError::Limit("nodes"))?;
    if depth > 16 {
        return Err(SnapshotError::Limit("encoding depth"));
    }
    match value {
        Json::Array(values) => {
            for value in values {
                check_json(value, depth + 1, remaining)?;
            }
        }
        Json::Object(values) => {
            for value in values.values() {
                check_json(value, depth + 1, remaining)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn children(contract: &Contract) -> Vec<&Arc<Contract>> {
    match contract.kind() {
        ContractKind::Record(fields) => fields.values().map(|f| &f.contract).collect(),
        ContractKind::List(c)
        | ContractKind::Option(c)
        | ContractKind::Iter(c)
        | ContractKind::Dataset(c) => vec![c],
        ContractKind::Map(a, b) | ContractKind::Union(a, b) => vec![a, b],
        ContractKind::Scalar(_) | ContractKind::Unknown => vec![],
    }
}
struct Capture {
    limits: SnapshotLimits,
    seen: HashSet<usize>,
    nodes: Vec<Node>,
    charge: usize,
    patterns: usize,
}
impl Capture {
    fn charge(&mut self, bytes: usize) -> Result<(), SnapshotError> {
        self.charge = self
            .charge
            .checked_add(bytes)
            .filter(|n| *n <= self.limits.bytes)
            .ok_or(SnapshotError::Limit("bytes"))?;
        Ok(())
    }
    fn visit(&mut self, contract: &Contract, depth: usize) -> Result<(), SnapshotError> {
        if depth > self.limits.depth {
            return Err(SnapshotError::Limit("depth"));
        }
        let address = std::ptr::from_ref(contract) as usize;
        if !self.seen.insert(address) {
            return Ok(());
        }
        if self.seen.len() > self.limits.nodes {
            return Err(SnapshotError::Limit("nodes"));
        }
        // Reserve expansion/escaping before constructing canonical containers.
        self.charge(1024)?;
        self.charge(
            contract
                .name()
                .len()
                .checked_mul(6)
                .ok_or(SnapshotError::Limit("bytes"))?,
        )?;
        if let ContractKind::Record(fields) = contract.kind() {
            for name in fields.keys() {
                self.charge(name.len().saturating_mul(6).saturating_add(128))?;
            }
        }
        for value in &contract.constraints().enumeration {
            let bytes = match value {
                Data::Text(text) => text.len(),
                Data::Int(_) => 20,
                Data::Bool(_) => 5,
                Data::Decimal(value) => usize::try_from(value.compact_text_size_bound())
                    .map_err(|_| SnapshotError::Limit("bytes"))?,
                _ => return Err(SnapshotError::Invalid),
            };
            self.charge(bytes.saturating_mul(6).saturating_add(64))?;
        }
        for bound in [&contract.constraints().min, &contract.constraints().max]
            .into_iter()
            .flatten()
        {
            let bytes = usize::try_from(bound.as_bigint_and_scale().0.bits())
                .map_err(|_| SnapshotError::Limit("bytes"))?;
            self.charge(bytes.saturating_add(2048))?;
        }
        for pattern in &contract.constraints().patterns {
            self.patterns += 1;
            if self.patterns > self.limits.patterns {
                return Err(SnapshotError::Limit("patterns"));
            }
            self.charge(pattern.as_str().len().saturating_mul(6).saturating_add(64))?;
        }
        for member in contract.display().enum_tones().keys() {
            self.charge(member.len().saturating_mul(6).saturating_add(64))?;
        }
        for child in children(contract) {
            self.visit(child, depth + 1)?;
        }
        let digest = contract.digest();
        // Equal contracts from separate Arcs remain one schema node.
        if !self.nodes.iter().any(|n| n.digest == digest) {
            self.nodes.push(Node {
                digest: digest.into(),
                canonical: contract.canonical(),
            });
        }
        Ok(())
    }
}
fn visit_digests(contract: &Contract, visited: &mut BTreeSet<String>) {
    if visited.insert(contract.digest().into()) {
        for child in children(contract) {
            visit_digests(child, visited);
        }
    }
}
fn text(value: &Json) -> Result<&str, SnapshotError> {
    value.as_str().ok_or(SnapshotError::Invalid)
}
fn array(value: &Json) -> Result<&Vec<Json>, SnapshotError> {
    value.as_array().ok_or(SnapshotError::Invalid)
}
fn primitive(value: &Json) -> Result<Primitive, SnapshotError> {
    Ok(match text(value)? {
        "Text" => Primitive::Text,
        "Int" => Primitive::Int,
        "Decimal" => Primitive::Decimal,
        "Bool" => Primitive::Bool,
        "Bytes" => Primitive::Bytes,
        "Instant" => Primitive::Instant,
        "Duration" => Primitive::Duration,
        "Interval" => Primitive::Interval,
        _ => return Err(SnapshotError::Invalid),
    })
}
fn restore(
    value: &Json,
    resolved: &BTreeMap<String, (Arc<Contract>, usize)>,
    limits: SnapshotLimits,
    patterns: &mut usize,
) -> Result<(Contract, usize), SnapshotError> {
    let v = array(value)?;
    if v.len() != 14 || v[0] != "wes.contract" || v[1] != 1 {
        return Err(SnapshotError::Invalid);
    }
    let name = text(&v[2])?;
    if name.is_empty() || name.encode_utf16().count() > 4096 {
        return Err(SnapshotError::Invalid);
    }
    let base_digest = if v[3].is_null() {
        None
    } else {
        let digest = text(&v[3])?;
        if !valid_digest(digest) {
            return Err(SnapshotError::Invalid);
        }
        Some(digest.to_owned())
    };
    let display = v[4].as_object().ok_or(SnapshotError::Invalid)?;
    if display.len() != 1 {
        return Err(SnapshotError::Invalid);
    }
    let tones = display
        .get("enumTones")
        .and_then(Json::as_object)
        .ok_or(SnapshotError::Invalid)?;
    let mut enum_tones = BTreeMap::new();
    for (member, tone) in tones {
        enum_tones.insert(
            member.clone(),
            EnumTone::parse(text(tone)?).ok_or(SnapshotError::Invalid)?,
        );
    }
    let k = array(&v[5])?;
    let tag = text(k.first().ok_or(SnapshotError::Invalid)?)?;
    let mut depth = 0usize;
    let mut child = |value: &Json| -> Result<Arc<Contract>, SnapshotError> {
        let (contract, d) = resolved.get(text(value)?).ok_or(SnapshotError::Invalid)?;
        depth = depth.max(d.checked_add(1).ok_or(SnapshotError::Limit("depth"))?);
        Ok(contract.clone())
    };
    let kind = match (tag, k.len()) {
        ("scalar", 2) => ContractKind::Scalar(primitive(&k[1])?),
        ("unknown", 1) => ContractKind::Unknown,
        ("record", 2) => {
            let mut fields = IndexMap::new();
            for entry in array(&k[1])? {
                let e = array(entry)?;
                if e.len() != 3 {
                    return Err(SnapshotError::Invalid);
                }
                let key = text(&e[0])?;
                if key.len() > limits.bytes
                    || fields
                        .insert(
                            key.to_owned(),
                            ContractField {
                                contract: child(&e[2])?,
                                optional: e[1].as_bool().ok_or(SnapshotError::Invalid)?,
                            },
                        )
                        .is_some()
                {
                    return Err(SnapshotError::Invalid);
                }
            }
            ContractKind::Record(fields)
        }
        ("list", 2) => ContractKind::List(child(&k[1])?),
        ("option", 2) => ContractKind::Option(child(&k[1])?),
        ("iter", 2) => ContractKind::Iter(child(&k[1])?),
        ("dataset", 2) => ContractKind::Dataset(child(&k[1])?),
        ("map", 3) => ContractKind::Map(child(&k[1])?, child(&k[2])?),
        ("union", 3) => ContractKind::Union(child(&k[1])?, child(&k[2])?),
        _ => return Err(SnapshotError::Invalid),
    };
    if depth > limits.depth {
        return Err(SnapshotError::Limit("depth"));
    }
    let mut enumeration = Vec::new();
    for member in array(&v[6])? {
        let ContractKind::Scalar(p) = &kind else {
            return Err(SnapshotError::Invalid);
        };
        let data = literals::read(text(member)?, &crate::Shape::Primitive(*p))
            .ok_or(SnapshotError::Invalid)?;
        enumeration.push(data);
    }
    fn numeric(v: &Json) -> Result<Option<bigdecimal::BigDecimal>, SnapshotError> {
        if v.is_null() {
            return Ok(None);
        }
        text(v)?
            .parse()
            .map(Some)
            .map_err(|_| SnapshotError::Invalid)
    }
    fn count(v: &Json) -> Result<Option<usize>, SnapshotError> {
        if v.is_null() {
            return Ok(None);
        }
        v.as_u64()
            .filter(|n| *n <= i32::MAX as u64)
            .map(|n| Some(n as usize))
            .ok_or(SnapshotError::Invalid)
    }
    let mut regexes = Vec::new();
    for pattern in array(&v[13])? {
        *patterns += 1;
        let pattern = text(pattern)?;
        if *patterns > limits.patterns || pattern.len() > 16384 {
            return Err(SnapshotError::Limit("patterns"));
        }
        regexes.push(
            regex::RegexBuilder::new(pattern)
                .size_limit(1024 * 1024)
                .build()
                .map_err(|_| SnapshotError::Invalid)?,
        );
    }
    let constraints = ContractLimits {
        enumeration,
        min: numeric(&v[7])?,
        max: numeric(&v[8])?,
        min_length: count(&v[9])?,
        max_length: count(&v[10])?,
        min_items: count(&v[11])?,
        max_items: count(&v[12])?,
        patterns: regexes,
    };
    if matches!((&constraints.min,&constraints.max),(Some(a),Some(b)) if a>b)
        || matches!((constraints.min_length,constraints.max_length),(Some(a),Some(b)) if a>b)
        || matches!((constraints.min_items,constraints.max_items),(Some(a),Some(b)) if a>b)
    {
        return Err(SnapshotError::Invalid);
    }
    let contract = Contract {
        name: name.into(),
        kind,
        limits: constraints,
        display: ContractDisplay { enum_tones },
        base_digest,
        digest: Default::default(),
    };
    // Constraints must apply to the declared kind, rather than being silently ignored.
    let c = contract.constraints();
    if ((c.min.is_some() || c.max.is_some())
        && !matches!(
            contract.kind(),
            ContractKind::Scalar(Primitive::Int | Primitive::Decimal)
        ))
        || ((c.min_length.is_some() || c.max_length.is_some() || !c.patterns.is_empty())
            && !matches!(contract.kind(), ContractKind::Scalar(Primitive::Text)))
        || ((c.min_items.is_some() || c.max_items.is_some())
            && !matches!(contract.kind(), ContractKind::List(_)))
        || c.enumeration.iter().any(|d| !contract.issues(d).is_empty())
    {
        return Err(SnapshotError::Invalid);
    }
    for member in contract.display.enum_tones.keys() {
        if !c
            .enumeration
            .iter()
            .any(|d| enum_spelling(d).as_ref() == Some(member))
        {
            return Err(SnapshotError::Invalid);
        }
    }
    Ok((contract, depth))
}
fn valid_digest(text: &str) -> bool {
    text.len() == 71
        && text.starts_with("sha256:")
        && text[7..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn encode(wire: &Wire, limit: usize) -> Result<Vec<u8>, SnapshotError> {
    struct Bounded {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self
                .bytes
                .len()
                .checked_add(bytes.len())
                .is_none_or(|n| n > self.limit)
            {
                return Err(std::io::Error::other("schema byte limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded {
        bytes: vec![],
        limit,
    };
    serde_json::to_writer(&mut writer, wire).map_err(|_| SnapshotError::Limit("bytes"))?;
    Ok(writer.bytes)
}
