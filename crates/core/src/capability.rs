//! Immutable provider metadata. Execution handles belong to the engine, not this catalogue.
mod evidence;
use crate::{Data, Primitive, Provenance, Shape};
use indexmap::IndexMap;
use std::{collections::BTreeSet, sync::Arc};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Sort {
    #[default]
    Plain,
    Selector(String),
    /// Runtime data with retained-resource suggestions; references remain valid inputs.
    Resource(String),
    Fresh(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parameter {
    pub name: String,
    pub shape: Shape,
    pub required: bool,
    pub content: Option<String>,
    pub sort: Sort,
    /// Inert bounded hints projected by the producer from its resolved contract.
    pub constraints: Vec<String>,
    /// Full declared scalar domain; previews never replace contract validation.
    pub enum_domain: Option<EnumDomain>,
}
impl Parameter {
    pub fn new(name: impl Into<String>, shape: Shape, required: bool) -> Self {
        Self {
            name: name.into(),
            shape,
            required,
            content: None,
            sort: Sort::Plain,
            constraints: vec![],
            enum_domain: None,
        }
    }
    pub fn constrained_by(mut self, contract: &crate::contracts::Contract) -> Self {
        self.constraints = contract.constraint_hints();
        self.enum_domain = EnumDomain::from_contract(contract);
        self
    }
    pub fn selecting(mut self, registry: impl Into<String>) -> Self {
        self.sort = Sort::Selector(registry.into());
        self
    }
    pub fn suggesting(mut self, registry: impl Into<String>) -> Self {
        self.sort = Sort::Resource(registry.into());
        self
    }
    pub fn naming(mut self, namespace: impl Into<String>) -> Self {
        self.sort = Sort::Fresh(namespace.into());
        self
    }
    pub fn written_in(mut self, language: impl Into<String>) -> Self {
        self.content = Some(language.into());
        self
    }
}

/// Immutable finite metadata captured from a resolved scalar contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumDomain {
    pub kind: EnumKind,
    pub members: Arc<[Data]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnumKind {
    Text,
    Int,
    Decimal,
    Bool,
}
impl EnumKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Int => "int",
            Self::Decimal => "decimal",
            Self::Bool => "bool",
        }
    }
}

/// Exact scalar spelling, shared with the runtime semantics of documented OneOf rules.
pub fn enum_spelling(data: &Data) -> Option<String> {
    match data {
        Data::Text(text) => Some(text.to_string()),
        Data::Int(value) => Some(value.to_string()),
        Data::Decimal(value) => Some(value.to_string()),
        Data::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumChoices {
    pub kind: EnumKind,
    pub members: Vec<String>,
    pub total: usize,
    pub complete: bool,
}

impl EnumDomain {
    pub const MAX_MEMBERS: usize = 64;
    pub const MAX_ENCODED_BYTES: usize = 16 * 1024;

    pub fn from_contract(contract: &crate::contracts::Contract) -> Option<Self> {
        use crate::contracts::ContractKind;
        let kind = match contract.kind() {
            ContractKind::Scalar(Primitive::Text) => EnumKind::Text,
            ContractKind::Scalar(Primitive::Int) => EnumKind::Int,
            ContractKind::Scalar(Primitive::Decimal) => EnumKind::Decimal,
            ContractKind::Scalar(Primitive::Bool) => EnumKind::Bool,
            _ => return None,
        };
        let enumeration = &contract.constraints().enumeration;
        if enumeration.is_empty() {
            return None;
        }
        // Preserve declaration order, keeping the first occurrence of each exact spelling.
        let mut seen = BTreeSet::new();
        let members = enumeration
            .iter()
            .filter(|data| enum_spelling(data).is_some_and(|text| seen.insert(text)))
            .cloned()
            .collect::<Vec<_>>()
            .into();
        Some(Self { kind, members })
    }

    /// Intersect the FULL domain before counting or cutting a bounded prefix.
    /// Unrelated and inferred rules do not constrain declared suggestions.
    pub fn choices(&self, key: &str, rules: &[DeclaredRule]) -> EnumChoices {
        let sets: Vec<_> = rules
            .iter()
            .filter(|rule| rule.is_binding())
            .filter_map(|rule| match &rule.rule {
                Rule::OneOf { key: name, values } if name == key => Some(values),
                _ => None,
            })
            .collect();
        let effective = || {
            self.members.iter().filter_map(|data| {
                let text = enum_spelling(data)?;
                sets.iter()
                    .all(|values| values.contains(&text))
                    .then_some(text)
            })
        };
        let total = effective().count();
        // Charge the entire compact JSON choices object, including escaped strings.
        // `false` is one byte longer than `true`, so this also bounds complete previews.
        let mut bytes = format!(
            "{{\"kind\":\"{}\",\"members\":[],\"total\":{total},\"complete\":false}}",
            self.kind.name()
        )
        .len();
        let mut members = Vec::new();
        for text in effective().take(Self::MAX_MEMBERS) {
            let charge = json_string_bytes(&text) + usize::from(!members.is_empty());
            if bytes + charge > Self::MAX_ENCODED_BYTES {
                break;
            }
            bytes += charge;
            members.push(text);
        }
        EnumChoices {
            kind: self.kind,
            complete: members.len() == total,
            members,
            total,
        }
    }
}

fn json_string_bytes(text: &str) -> usize {
    2 + text
        .chars()
        .map(|c| match c {
            '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
            '\u{0}'..='\u{1f}' => 6,
            _ => c.len_utf8(),
        })
        .sum::<usize>()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Typing {
    pub shape: Shape,
    pub provenance: Provenance,
}
impl Typing {
    pub fn new(shape: Shape) -> Self {
        Self {
            shape,
            provenance: Provenance::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Safety {
    Safe,
    Unsafe,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rule {
    MutuallyExclusive(BTreeSet<String>),
    Requires {
        key: String,
        needs: String,
    },
    OneOf {
        key: String,
        values: BTreeSet<String>,
    },
    ProvenanceFact {
        key: String,
        fact: String,
        expected: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleBasis {
    Documented { note: Option<String> },
    Inferred { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeclaredRule {
    pub rule: Rule,
    pub basis: RuleBasis,
}
impl DeclaredRule {
    pub fn is_binding(&self) -> bool {
        matches!(self.basis, RuleBasis::Documented { .. })
    }
}

/// Declarative projection of an explicitly acquired result into resource suggestions.
/// Fields name direct record members; completion never invokes the producer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceProjection {
    pub registry: String,
    pub rows: String,
    pub key: String,
    pub label: String,
    pub detail: String,
    pub observed_at: String,
}
impl ResourceProjection {
    pub fn fields(&self) -> [&str; 6] {
        [
            &self.registry,
            &self.rows,
            &self.key,
            &self.label,
            &self.detail,
            &self.observed_at,
        ]
    }
}

/// Mutable while describing an import, frozen and validated by ProviderDescription::new.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capability {
    pub resources: Option<ResourceProjection>,
    pub path: Vec<String>,
    pub summary: String,
    pub parameters: Vec<Parameter>,
    pub result: Shape,
    pub rules: Vec<DeclaredRule>,
    pub safety: Safety,
    pub provenance_arguments: BTreeSet<String>,
    pub streaming: bool,
}
impl Capability {
    pub fn new(
        path: impl IntoIterator<Item = impl Into<String>>,
        result: Shape,
        safety: Safety,
    ) -> Self {
        Self {
            resources: None,
            path: path.into_iter().map(Into::into).collect(),
            summary: String::new(),
            parameters: Vec::new(),
            result,
            rules: Vec::new(),
            safety,
            provenance_arguments: BTreeSet::new(),
            streaming: false,
        }
    }
    pub fn parameter(&self, name: &str) -> Option<&Parameter> {
        self.parameters.iter().find(|p| p.name == name)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("invalid provider metadata: {0}")]
pub struct MetadataError(pub String);

#[derive(Clone, Debug)]
pub struct ProviderDescription {
    name: String,
    capabilities: IndexMap<Vec<String>, Arc<Capability>>,
    secrets: Vec<String>,
    information: Option<crate::Value>,
}
impl ProviderDescription {
    pub fn new(
        name: impl Into<String>,
        capabilities: impl IntoIterator<Item = Capability>,
        secrets: Vec<String>,
    ) -> Result<Self, MetadataError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(MetadataError("a provider needs a name".into()));
        }
        let mut entries = IndexMap::new();
        for capability in capabilities {
            if capability.resources.as_ref().is_some_and(|r| {
                r.fields()
                    .iter()
                    .any(|f| f.is_empty() || f.len() > 128 || f.chars().any(char::is_control))
            }) {
                return Err(MetadataError("invalid resource projection metadata".into()));
            }
            if capability.path.is_empty() || capability.path.iter().any(|part| part.is_empty()) {
                return Err(MetadataError("a capability needs a non-empty path".into()));
            }
            let mut parameters = BTreeSet::new();
            for parameter in &capability.parameters {
                if parameter.name.is_empty() || !parameters.insert(&parameter.name) {
                    return Err(MetadataError(format!(
                        "empty or duplicate parameter: {}",
                        parameter.name
                    )));
                }
            }
            for rule in &capability.rules {
                if matches!(&rule.basis,RuleBasis::Inferred{reason} if reason.trim().is_empty()) {
                    return Err(MetadataError("an inferred rule requires its reason".into()));
                }
            }
            if entries
                .insert(capability.path.clone(), Arc::new(capability))
                .is_some()
            {
                return Err(MetadataError("duplicate capability path".into()));
            }
        }
        Ok(Self {
            name,
            capabilities: entries,
            secrets,
            information: None,
        })
    }
    /// Inert documentation retained with the provider, never execution authority.
    pub fn with_information(mut self, information: crate::Data) -> Self {
        self.information = Some(
            crate::Value::new(Shape::Unknown, information, crate::Provenance::default())
                .expect("unknown documentation shape"),
        );
        self
    }
    pub fn information(&self) -> Option<&crate::Data> {
        self.information.as_ref().map(crate::Value::data)
    }
    /// A producer-validated documentation contract; still inert, never authority.
    pub fn with_typed_information(mut self, information: crate::Value) -> Self {
        self.information = Some(information);
        self
    }
    pub fn information_value(&self) -> Option<&crate::Value> {
        self.information.as_ref()
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn capabilities(&self) -> impl ExactSizeIterator<Item = &Arc<Capability>> {
        self.capabilities.values()
    }
    pub fn capability(&self, path: &[String]) -> Option<&Arc<Capability>> {
        self.capabilities.get(path)
    }
    pub fn secrets(&self) -> &[String] {
        &self.secrets
    }
    pub fn renamed(&self, name: impl Into<String>) -> Result<Self, MetadataError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(MetadataError("a provider needs a name".into()));
        }
        Ok(Self {
            name,
            ..self.clone()
        })
    }
}

/// Owner-controlled mutation permits atomic snapshots without embedding a concurrency policy.
#[derive(Clone, Debug, Default)]
pub struct Catalogue {
    providers: IndexMap<String, Arc<ProviderDescription>>,
}
impl Catalogue {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn register(&mut self, provider: ProviderDescription) -> Option<Arc<ProviderDescription>> {
        self.providers
            .insert(provider.name.clone(), Arc::new(provider))
    }
    pub fn unregister(&mut self, name: &str) -> Option<Arc<ProviderDescription>> {
        self.providers.shift_remove(name)
    }
    pub fn provider(&self, name: &str) -> Option<&Arc<ProviderDescription>> {
        self.providers.get(name)
    }
    pub fn provider_names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.providers.keys().map(String::as_str)
    }
    pub fn resolve(&self, path: &[String]) -> Option<&Arc<Capability>> {
        let (head, tail) = path.split_first()?;
        self.provider(head)?.capability(tail)
    }
    pub fn clear(&mut self) {
        self.providers.clear();
    }
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}
