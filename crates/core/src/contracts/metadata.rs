//! Captured declaration metadata, independent of structural typing and observed data.
use super::{Contract, ContractKind};
use crate::capability::{EnumDomain, enum_spelling};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_DESCRIPTORS: usize = 128;
pub const MAX_BYTES: usize = 64 * 1024;
pub const MAX_DESCRIPTOR_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractIdentity {
    name: String,
    digest: String,
}
impl ContractIdentity {
    fn of(c: &Contract) -> Self {
        Self {
            name: c.name().into(),
            digest: c.digest().into(),
        }
    }
    fn valid(&self) -> bool {
        !self.name.is_empty()
            && self.name.encode_utf16().count() <= 4096
            && self.digest.len() == 71
            && self.digest.starts_with("sha256:")
            && self.digest[7..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldDescriptor {
    contract: ContractIdentity,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    members: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    complete: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tones: Option<BTreeMap<String, super::EnumTone>>,
}
impl FieldDescriptor {
    fn of(c: &Contract) -> Self {
        let kind = match c.kind() {
            ContractKind::Scalar(p) => p.to_string().to_ascii_lowercase(),
            ContractKind::Record(_) => "record".into(),
            ContractKind::List(_) => "list".into(),
            ContractKind::Option(_) => "option".into(),
            ContractKind::Map(..) => "map".into(),
            ContractKind::Iter(_) => "iter".into(),
            ContractKind::Dataset(_) => "dataset".into(),
            ContractKind::Union(..) => "union".into(),
            ContractKind::Unknown => "unknown".into(),
        };
        let mut result = Self {
            contract: ContractIdentity::of(c),
            kind,
            source: None,
            members: None,
            total: None,
            complete: None,
            tones: None,
        };
        if let Some(domain) = EnumDomain::from_contract(c) {
            let preview = domain.choices("", &[]);
            result.source = Some("validated".into());
            if !c.display().enum_tones().is_empty() {
                result.tones = Some(c.display().enum_tones().clone());
            }
            result.total = Some(preview.total);
            result.complete = Some(false);
            result.members = Some(vec![]);
            for member in preview.members {
                result.members.as_mut().unwrap().push(member);
                if serde_json::to_vec(&result).unwrap().len() > MAX_DESCRIPTOR_BYTES {
                    result.members.as_mut().unwrap().pop();
                    break;
                }
            }
            result.complete = Some(result.members.as_ref().unwrap().len() == preview.total);
        }
        result
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValueMetadata {
    version: u32,
    contract: ContractIdentity,
    truncated: bool,
    fields: BTreeMap<String, FieldDescriptor>,
}
pub fn field_segment(key: &str) -> String {
    format!("/f:{}", key.replace('~', "~0").replace('/', "~1"))
}
fn valid_path(path: &str) -> bool {
    if path.is_empty() {
        return true;
    }
    let Some(path) = path.strip_prefix('/') else {
        return false;
    };
    let parts: Vec<_> = path.split('/').collect();
    parts.len() <= 64
        && parts.iter().all(|p| {
            if *p == "e" || *p == "o" {
                return true;
            }
            let Some(field) = p.strip_prefix("f:") else {
                return false;
            };
            let mut chars = field.chars();
            while let Some(c) = chars.next() {
                if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
                    return false;
                }
            }
            true
        })
}
impl ValueMetadata {
    pub fn capture(contract: &Contract) -> Self {
        let mut result = Self {
            version: 1,
            contract: ContractIdentity::of(contract),
            truncated: false,
            fields: BTreeMap::new(),
        };
        let mut stack = vec![(contract, String::new(), 0)];
        let mut visits: usize = 0;
        while let Some((c, path, depth)) = stack.pop() {
            visits += 1;
            if depth > 64 || visits > 100_000 || result.fields.len() >= MAX_DESCRIPTORS {
                result.truncated = true;
                break;
            }
            visits = visits.saturating_add(c.constraints().enumeration.len());
            if visits > 100_000 {
                result.truncated = true;
                break;
            }
            let descriptor = FieldDescriptor::of(c);
            if serde_json::to_vec(&descriptor).unwrap().len() > MAX_DESCRIPTOR_BYTES {
                result.truncated = true;
                continue;
            }
            result.fields.insert(path.clone(), descriptor);
            if serde_json::to_vec(&result).unwrap().len() > MAX_BYTES {
                result.fields.remove(&path);
                result.truncated = true;
                break;
            }
            match c.kind() {
                ContractKind::Record(fields) => {
                    // Bound pending work as well as completed visits.
                    for (key, field) in fields.iter().take(100_000 - visits).rev() {
                        stack.push((
                            &field.contract,
                            path.clone() + &field_segment(key),
                            depth + 1,
                        ));
                    }
                    if fields.len() > 100_000 - visits {
                        result.truncated = true;
                    }
                }
                ContractKind::List(inner) => stack.push((inner, path + "/e", depth + 1)),
                ContractKind::Option(inner) => stack.push((inner, path + "/o", depth + 1)),
                _ => {}
            }
        }
        result
    }
    /// The public metadata channel contains only scalar descriptors understood by readers.
    /// Container identities remain in the retained projection snapshot, never in `fields`.
    /// A name outside the reader's UTF-16 bound makes that identity unavailable.
    pub fn wire(&self) -> Option<Self> {
        if self.contract.name.encode_utf16().count() > 1024 {
            return None;
        }
        let mut wire = self.clone();
        wire.fields.retain(|_, d| {
            let supported = matches!(d.kind.as_str(), "text" | "int" | "decimal" | "bool");
            if supported && d.contract.name.encode_utf16().count() > 1024 {
                wire.truncated = true;
                return false;
            }
            supported
        });
        Some(wire)
    }
    pub fn needs_projection_snapshot(&self) -> bool {
        self.wire().as_ref() != Some(self)
    }
    /// Refuse retained public metadata that the browser would discard.
    pub fn validate_wire(&self) -> Result<(), &'static str> {
        self.validate()?;
        if self.wire().as_ref() != Some(self) {
            return Err("invalid public metadata kinds or identity names");
        }
        Ok(())
    }
    pub fn project(&self, path: &str) -> Option<Self> {
        if path.is_empty() {
            return Some(self.clone());
        }
        let contract = self.fields.get(path)?.contract.clone();
        let prefix = path.to_owned() + "/";
        let fields = self
            .fields
            .iter()
            .filter_map(|(p, d)| {
                (p == path || p.starts_with(&prefix)).then(|| (p[path.len()..].into(), d.clone()))
            })
            .collect();
        Some(Self {
            version: 1,
            contract,
            truncated: self.truncated,
            fields,
        })
    }
    /// Capture a proven record embedding of a validated contract. The wrapper identity
    /// binds its declared field to the child's digest; no observed fields are inferred.
    pub fn record_wrapper(name: &str, field: &str, contract: &Contract) -> Self {
        let wrapper = Contract {
            name: name.into(),
            kind: ContractKind::Record(
                [(
                    field.into(),
                    super::ContractField {
                        contract: std::sync::Arc::new(contract.clone()),
                        optional: false,
                    },
                )]
                .into(),
            ),
            limits: Default::default(),
            display: Default::default(),
            base_digest: None,
            digest: Default::default(),
        };
        Self::capture(&wrapper)
    }
    /// JSON node charge shared with the retained decoder (object keys are not nodes).
    pub fn nodes(&self) -> usize {
        let wire_nodes = if self.needs_projection_snapshot() {
            self.wire().map_or(0, |wire| wire.description_nodes())
        } else {
            0
        };
        self.description_nodes() + wire_nodes
    }
    fn description_nodes(&self) -> usize {
        7 + self
            .fields
            .values()
            .map(|d| {
                5 + usize::from(d.source.is_some())
                    + d.members.as_ref().map_or(0, |m| 1 + m.len())
                    + usize::from(d.total.is_some())
                    + usize::from(d.complete.is_some())
                    + d.tones.as_ref().map_or(0, |t| 1 + t.len())
            })
            .sum::<usize>()
    }
    pub fn charge(&self) -> u64 {
        let mut bytes = serde_json::to_vec(self).unwrap().len();
        if self.needs_projection_snapshot() {
            bytes += self
                .wire()
                .map_or(0, |wire| serde_json::to_vec(&wire).unwrap().len());
        }
        bytes as u64 * 6 + self.fields.len() as u64 * 256
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1
            || !self.contract.valid()
            || self.fields.len() > MAX_DESCRIPTORS
            || serde_json::to_vec(self).unwrap().len() > MAX_BYTES
        {
            return Err("invalid or oversized value metadata");
        }
        for (path, d) in &self.fields {
            if !valid_path(path)
                || !d.contract.valid()
                || !matches!(
                    d.kind.as_str(),
                    "text"
                        | "int"
                        | "decimal"
                        | "bool"
                        | "dataset"
                        | "instant"
                        | "duration"
                        | "interval"
                        | "bytes"
                        | "record"
                        | "list"
                        | "option"
                        | "map"
                        | "iter"
                        | "union"
                        | "unknown"
                )
                || serde_json::to_vec(d).unwrap().len() > MAX_DESCRIPTOR_BYTES
            {
                return Err("invalid metadata descriptor");
            }
            if let Some((parent, segment)) = path.rsplit_once('/')
                && let Some(parent) = self.fields.get(parent)
                && match segment {
                    "e" => parent.kind != "list",
                    "o" => parent.kind != "option",
                    _ => parent.kind != "record",
                }
            {
                return Err("metadata path disagrees with its declared container kind");
            }
            if let Some(tones) = &d.tones {
                if tones.len() > 64 || tones.len() > d.total.unwrap_or(0) || d.members.is_none() {
                    return Err("invalid metadata tones");
                }
                for member in tones.keys() {
                    if !valid_member(&d.kind, member)
                        || (d.complete == Some(true)
                            && !d.members.as_ref().unwrap().contains(member))
                    {
                        return Err("invalid metadata tone member");
                    }
                }
            }
            match (&d.members, d.total, d.complete, d.source.as_deref()) {
                (None, None, None, None) => {}
                (Some(m), Some(total), Some(complete), Some("validated"))
                    if m.len() <= 64
                        && total > 0
                        && total <= 9_007_199_254_740_991
                        && total >= m.len()
                        && complete == (total == m.len()) =>
                {
                    let mut seen = BTreeSet::new();
                    for member in m {
                        let valid = valid_member(&d.kind, member);
                        if !valid || !seen.insert(member) {
                            return Err("invalid metadata enum member");
                        }
                    }
                }
                _ => return Err("invalid metadata enum counts or source"),
            }
        }
        if self
            .fields
            .get("")
            .is_some_and(|d| d.contract != self.contract)
        {
            return Err("inconsistent metadata root identity");
        }
        Ok(())
    }
}

impl Contract {
    /// Exact digest input, shared by resolved snapshots; no independent schema grammar.
    pub(super) fn canonical(&self) -> serde_json::Value {
        let kind = match self.kind() {
            ContractKind::Scalar(p) => serde_json::json!(["scalar", p.to_string()]),
            ContractKind::Record(fields) => serde_json::json!([
                "record",
                fields
                    .iter()
                    .map(|(k, f)| serde_json::json!([k, f.optional, f.contract.digest()]))
                    .collect::<Vec<_>>()
            ]),
            ContractKind::List(c) => serde_json::json!(["list", c.digest()]),
            ContractKind::Option(c) => serde_json::json!(["option", c.digest()]),
            ContractKind::Iter(c) => serde_json::json!(["iter", c.digest()]),
            ContractKind::Dataset(c) => serde_json::json!(["dataset", c.digest()]),
            ContractKind::Map(a, b) => serde_json::json!(["map", a.digest(), b.digest()]),
            ContractKind::Union(a, b) => serde_json::json!(["union", a.digest(), b.digest()]),
            ContractKind::Unknown => serde_json::json!(["unknown"]),
        };
        let l = self.constraints();
        serde_json::json!([
            "wes.contract",
            1,
            self.name(),
            self.base_digest,
            self.display(),
            kind,
            l.enumeration.iter().map(enum_spelling).collect::<Vec<_>>(),
            l.min.as_ref().map(ToString::to_string),
            l.max.as_ref().map(ToString::to_string),
            l.min_length,
            l.max_length,
            l.min_items,
            l.max_items,
            l.patterns.iter().map(|p| p.as_str()).collect::<Vec<_>>()
        ])
    }
    /// Digest v1: canonical resolved content, never a registry lookup or source path.
    pub fn digest(&self) -> &str {
        self.digest.get_or_init(|| {
            use sha2::{Digest, Sha256};
            let canonical = self.canonical();
            format!(
                "sha256:{:x}",
                Sha256::digest(serde_json::to_vec(&canonical).unwrap())
            )
        })
    }
}

fn valid_member(kind: &str, member: &str) -> bool {
    match kind {
        "text" => true,
        "int" => member
            .parse::<i64>()
            .ok()
            .is_some_and(|x| x.to_string() == member),
        "decimal" => member
            .parse::<crate::Decimal>()
            .ok()
            .is_some_and(|x| enum_spelling(&crate::Data::Decimal(x)).as_deref() == Some(member)),
        "bool" => matches!(member, "true" | "false"),
        _ => false,
    }
}
pub(super) fn display_fits(contract: &Contract) -> bool {
    let mut descriptor = FieldDescriptor::of(contract);
    if descriptor.members.is_some() {
        descriptor.members = Some(vec![]);
        descriptor.complete = Some(false);
    }
    serde_json::to_vec(&descriptor).unwrap().len() <= MAX_DESCRIPTOR_BYTES
}
