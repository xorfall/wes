//! A view package describes values and presentation, never execution authority.
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};
use wes_core::{
    Data,
    contracts::{Contract, ContractKind, ContractRegistry},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub id: String,
    pub summary: String,
    pub renderer: String,
    pub input: String,
    pub layout: Option<ViewLayout>,
    #[serde(deserialize_with = "unique_outputs")]
    pub outputs: BTreeMap<String, Port>,
    pub interaction: Option<Interaction>,
    #[serde(default, deserialize_with = "unique_slots")]
    pub slots: BTreeMap<String, Slot>,
}
/// Host-owned sizing in display columns and rows, independently for each tier.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ViewSize {
    pub columns: u16,
    pub rows: u16,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ViewTier {
    pub min: ViewSize,
    pub preferred: ViewSize,
    pub max: ViewSize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<ViewPlacement>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ViewPlacement {
    pub width: WidthPolicy,
    pub align: ViewAlignment,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WidthPolicy {
    Fill,
    Preferred,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewAlignment {
    Start,
    Center,
    End,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ViewLayout {
    pub preview: ViewTier,
    pub expanded: ViewTier,
    pub window: ViewTier,
}
impl ViewLayout {
    fn validate(&self) -> Result<(), String> {
        for tier in [&self.preview, &self.expanded, &self.window] {
            if tier.min.columns == 0
                || tier.min.rows == 0
                || tier.min.columns > tier.preferred.columns
                || tier.preferred.columns > tier.max.columns
                || tier.min.rows > tier.preferred.rows
                || tier.preferred.rows > tier.max.rows
                || tier.max.columns > 512
                || tier.max.rows > 200
            {
                return Err("Invalid view layout: require 1 <= min <= preferred <= max, at most 512 columns and 200 rows".into());
            }
        }
        Ok(())
    }
}
/// Membership is a connection between view identities, never a nested data value.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    pub accepts: Vec<String>,
    pub protocol: Option<String>,
    pub max: usize,
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub coordinates: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    pub r#type: String,
    pub mode: Mode,
    #[serde(default)]
    pub shared: bool,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    State,
    Event,
}
fn unique_outputs<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, Port>, D::Error> {
    unique_map(d)
}
fn unique_slots<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, Slot>, D::Error> {
    unique_map(d)
}
fn unique_map<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<String, T>, D::Error> {
    struct Unique<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Unique<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("unique port names")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, T>()? {
                if result.len() >= 32 || result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate or too many port names"));
                }
            }
            Ok(result)
        }
    }
    d.deserialize_map(Unique(std::marker::PhantomData))
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Interaction {
    pub protocol: String,
    pub state: String,
    pub event: String,
    #[serde(default, rename = "sharedFields")]
    pub shared_fields: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct Package {
    pub manifest: Manifest,
    pub digest: String,
    pub contracts: ContractRegistry,
    /// Exact renderer identity for dynamically installed code; built-ins ship with the host.
    pub artifact: Option<String>,
}
/// Cross-package contracts are validated at build time and when constructing a runtime catalogue.
pub fn validate_catalogue<'a>(
    packages: impl IntoIterator<Item = &'a Package>,
) -> Result<(), String> {
    let packages = packages.into_iter().collect::<Vec<_>>();
    if packages.len() > 64 {
        return Err("Too many view packages".into());
    }
    let mut names = std::collections::BTreeSet::new();
    let mut ids = std::collections::BTreeSet::new();
    let mut protocols = BTreeMap::new();
    for package in &packages {
        if !names.insert(package.manifest.name.as_str())
            || !ids.insert(package.manifest.id.as_str())
        {
            return Err("Duplicate view name/id".into());
        }
        if let Some(i) = &package.manifest.interaction {
            let identity = package.protocol_identity().expect("interaction");
            if protocols
                .insert(&i.protocol, identity.clone())
                .is_some_and(|old| old != identity)
            {
                return Err("Conflicting interaction protocol contracts".into());
            }
        }
    }
    for package in &packages {
        for slot in package.manifest.slots.values() {
            if slot
                .accepts
                .iter()
                .any(|name| !names.contains(name.as_str()))
            {
                return Err("View slot names an unavailable definition".into());
            }
            if slot
                .protocol
                .as_ref()
                .is_some_and(|p| !protocols.contains_key(p))
            {
                return Err("View slot names an unavailable protocol".into());
            }
            if slot.coordinates
                && package
                    .manifest
                    .interaction
                    .as_ref()
                    .is_none_or(|i| Some(&i.protocol) != slot.protocol.as_ref())
            {
                return Err("Coordinator must implement its slot protocol".into());
            }
        }
    }
    Ok(())
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.as_bytes()[0].is_ascii_alphabetic()
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}
impl Package {
    pub fn parse(manifest: &str, types: &str) -> Result<Self, String> {
        if manifest.len() > 64 * 1024 || types.len() > 256 * 1024 {
            return Err("View package is too large".into());
        }
        let m: Manifest =
            serde_json::from_str(manifest).map_err(|e| format!("Invalid view manifest: {e}"))?;
        if !identifier(&m.name)
            || m.id.is_empty()
            || m.id.len() > 128
            || !m
                .id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            || !m.id.as_bytes()[0].is_ascii_lowercase()
            || [
                "result", "json", "details", "source", "trace", "chart", "copy", "follow",
            ]
            .contains(&m.id.as_str())
        {
            return Err("Invalid or reserved view name/id".into());
        }
        if m.summary.is_empty() || m.summary.len() > 2048 || m.summary.chars().any(char::is_control)
        {
            return Err("View summary must be nonempty plain text (at most 2048 bytes)".into());
        }
        if !m.renderer.ends_with(".tsx")
            || m.renderer.contains('\\')
            || m.renderer
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
        {
            return Err("Renderer must be a relative .tsx file inside its package".into());
        }
        if let Some(layout) = &m.layout {
            layout.validate()?;
        }
        let mut contracts = ContractRegistry::new();
        contracts.load(types).map_err(|e| e.to_string())?;
        if m.outputs.len() > 32 || m.outputs.keys().any(|k| !identifier(k)) {
            return Err("Invalid output names or too many ports".into());
        }
        if m.slots.len() > 32
            || m.slots.values().filter(|s| s.default).count() > 1
            || m.slots.iter().any(|(name, slot)| {
                !identifier(name)
                    || m.outputs.contains_key(name)
                    || slot.max == 0
                    || slot.max > 128
                    || slot.accepts.len() > 64
                    || slot.accepts.iter().any(|s| !identifier(s))
                    || slot
                        .accepts
                        .iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        != slot.accepts.len()
                    || slot.protocol.as_ref().is_some_and(|s| !identifier(s))
                    || (slot.coordinates && slot.protocol.is_none())
            })
        {
            return Err("Invalid view slots: names, bounds and a single default are required; coordination requires a protocol".into());
        }
        let mut roots = vec![m.input.as_str()];
        roots.extend(m.outputs.values().map(|p| p.r#type.as_str()));
        if let Some(i) = &m.interaction {
            if !identifier(&i.protocol) {
                return Err("Invalid interaction protocol name".into());
            }
            let state = contracts.resolve(&i.state).map_err(|e| e.to_string())?;
            let ContractKind::Record(fields) = state.kind() else {
                return Err("Interaction state must be a record".into());
            };
            if i.shared_fields.len() > 32
                || i.shared_fields
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len()
                    != i.shared_fields.len()
                || i.shared_fields
                    .iter()
                    .any(|name| !fields.contains_key(name))
            {
                return Err(
                    "Shared interaction fields must name unique declared state fields".into(),
                );
            }
            roots.extend([i.state.as_str(), i.event.as_str()]);
        } else if !m.outputs.is_empty() {
            return Err("Output ports require an interaction definition".into());
        }
        if m.outputs.values().any(|p| p.shared)
            && m.interaction
                .as_ref()
                .is_none_or(|i| i.shared_fields.is_empty())
        {
            return Err("Shared outputs require declared committed state fields".into());
        }
        if m.outputs
            .values()
            .any(|p| p.mode == Mode::Event && !p.shared)
        {
            return Err("Event outputs require instance scope".into());
        }
        // Output and interaction authority is independent of the name also used by input.
        // Reusing the input contract must not make a Dataset-bearing output admissible.
        for name in m.outputs.values().map(|p| p.r#type.as_str()).chain(
            m.interaction
                .iter()
                .flat_map(|i| [i.state.as_str(), i.event.as_str()]),
        ) {
            if !contracts
                .resolve(name)
                .map_err(|e| e.to_string())?
                .shape()
                .is_inline()
            {
                return Err("View outputs and interaction state/events require inline data; Dataset is a read-only input port".into());
            }
        }
        let mut schemas = BTreeMap::new();
        let mut budget = 20_000;
        for name in roots {
            let contract = contracts.resolve(name).map_err(|e| e.to_string())?;
            project(contract.as_ref(), &mut schemas, &mut budget, 0)?;
        }
        if !matches!(
            contracts
                .resolve(&m.input)
                .map_err(|e| e.to_string())?
                .kind(),
            ContractKind::Record(_)
        ) {
            return Err("View input must be a record contract".into());
        }
        let digest = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&json!({"manifest":m,"contracts":schemas}))
                    .map_err(|e| e.to_string())?
            )
        );
        Ok(Self {
            manifest: m,
            digest,
            contracts,
            artifact: None,
        })
    }
    pub fn description(&self) -> Json {
        let mut schemas = BTreeMap::new();
        let mut names = vec![self.manifest.input.as_str()];
        names.extend(self.manifest.outputs.values().map(|p| p.r#type.as_str()));
        if let Some(i) = &self.manifest.interaction {
            names.extend([i.state.as_str(), i.event.as_str()]);
        }
        let mut budget = 20_000;
        for name in names {
            project(
                &self.contracts.resolve(name).expect("validated contract"),
                &mut schemas,
                &mut budget,
                0,
            )
            .expect("validated schema");
        }
        json!({"name":self.manifest.name,"id":self.manifest.id,"summary":self.manifest.summary,"digest":self.digest,"artifact":self.artifact,
            "eventWindow":{"items":32,"bytes":16384,"read":"List<T>","retention":"volatile","overflow":"drop-oldest-with-caution"},"input":self.manifest.input,"inputModes":["value"],"inputReferences":["unlinked","current","retained"],"inputDelivery":["finite","window"],"outputs":self.manifest.outputs,"outputScope":if self.manifest.outputs.values().any(|p|p.shared){"instance"}else{"local"},
            "layout":self.manifest.layout,"interaction":self.manifest.interaction,"contracts":schemas,"execution":"none","slots":self.manifest.slots})
    }
    pub fn input(&self) -> Arc<Contract> {
        self.contracts
            .resolve(&self.manifest.input)
            .expect("validated input")
    }
    pub fn validate_input(&self, data: &Data) -> Result<(), Vec<wes_core::ValidationIssue>> {
        let issues = self.input().issues(data);
        if issues.is_empty() {
            Ok(())
        } else {
            Err(issues)
        }
    }
    /// Protocol identity includes its reachable contracts, not unrelated package inputs.
    pub fn protocol_identity(&self) -> Option<String> {
        let interaction = self.manifest.interaction.as_ref()?;
        let mut schemas = BTreeMap::new();
        let mut budget = 20_000;
        for name in [&interaction.state, &interaction.event].into_iter().chain(
            self.manifest
                .outputs
                .values()
                .filter(|p| p.shared)
                .map(|p| &p.r#type),
        ) {
            project(
                &self.contracts.resolve(name).expect("validated protocol"),
                &mut schemas,
                &mut budget,
                0,
            )
            .expect("validated protocol projection");
        }
        Some(format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(
                    &json!({"protocol":interaction.protocol,"state":interaction.state,
                "event":interaction.event,"sharedFields":interaction.shared_fields,"outputs":self.manifest.outputs.iter().filter(|(_,p)|p.shared).collect::<BTreeMap<_,_>>(),"contracts":schemas})
                )
                .expect("protocol metadata")
            )
        ))
    }
}
fn project(
    c: &Contract,
    all: &mut BTreeMap<String, Json>,
    budget: &mut usize,
    depth: usize,
) -> Result<(), String> {
    project_read(c, all, budget, depth, false)
}
fn project_read(
    c: &Contract,
    all: &mut BTreeMap<String, Json>,
    budget: &mut usize,
    depth: usize,
    dataset_row: bool,
) -> Result<(), String> {
    // Dataset pages are validated by the native store against their exact
    // frozen element digest. A browser receives read-only rows and must never
    // reinterpret Wes regexes. Direct inputs and writable ports still refuse
    // patterns, even when this same name was reached from a Dataset first.
    let limits = c.constraints();
    if !dataset_row && !limits.patterns.is_empty() {
        return Err("Pattern-constrained view inputs require an explicit adapter; browser regex semantics are not Wes regex semantics".into());
    }
    if all.contains_key(c.name()) {
        return Ok(());
    }
    if depth > 64 || *budget == 0 {
        return Err("View contract expansion exceeds its budget".into());
    }
    *budget -= 1;
    let mut children = vec![];
    let mut schema = match c.kind() {
        ContractKind::Scalar(p) => json!({"kind":"scalar","primitive":p.to_string()}),
        ContractKind::Record(fields) => {
            children.extend(fields.values().map(|f|f.contract.as_ref()));
            json!({"kind":"record","fields":fields.iter().map(|(k,f)|(k.clone(),json!({"type":f.contract.name(),"optional":f.optional}))).collect::<BTreeMap<_,_>>()})
        }
        ContractKind::List(e) | ContractKind::Option(e) => {
            children.push(e.as_ref()); json!({"kind":if matches!(c.kind(),ContractKind::List(_)){"list"}else{"option"},"element":e.name()})
        }
        ContractKind::Union(a,b) => { children.extend([a.as_ref(),b.as_ref()]); json!({"kind":"union","alternatives":[a.name(),b.name()]}) }
        ContractKind::Dataset(e) => { children.push(e.as_ref()); json!({"kind":"dataset","element":e.name()}) }
        ContractKind::Unknown | ContractKind::Map(_,_) | ContractKind::Iter(_) => return Err("View ports require scalar/record/list/option/union or read-only Dataset input contracts; map, iterator and Unknown need an explicit adapter".into()),
    };
    schema["constraints"] = json!({"min":limits.min.as_ref().map(ToString::to_string),"max":limits.max.as_ref().map(ToString::to_string),
        "minLength":limits.min_length,"maxLength":limits.max_length,"minItems":limits.min_items,"maxItems":limits.max_items,
        "patterns":limits.patterns.iter().map(|p|p.as_str()).collect::<Vec<_>>(),
        "enum":limits.enumeration.iter().map(|d| match d {Data::Text(s)=>json!(s.as_ref()),Data::Bool(b)=>json!(b),Data::Int(n)=>json!(n.to_string()),Data::Decimal(n)=>json!(n.to_string()),_=>unreachable!("scalar constraints")}).collect::<Vec<_>>()});
    all.insert(c.name().into(), schema);
    for child in children {
        project_read(
            child,
            all,
            budget,
            depth + 1,
            dataset_row || matches!(c.kind(), ContractKind::Dataset(_)),
        )?;
    }
    Ok(())
}
