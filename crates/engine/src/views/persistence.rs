//! Workspace checkpoint metadata. Source payloads stay in the existing retained-value store.
use super::*;
use crate::{
    history::{HistoryImage, InvalidRecord},
    runtime::RunId,
    session::SessionStorage,
    workspace::Workspace,
};
use wes_core::{Provenance, Shape};
// A content check for the exact captured finite input, independent of stream run reuse.
// It is not an authority proof; retained-result history and definition identity remain required.
pub(crate) fn input_digest(value: &Value) -> String {
    use sha2::{Digest, Sha256};
    if value.provenance().policy().is_confidential()
        || crate::value_size::value_charge(value, 8 * 1024 * 1024).is_none()
    {
        return "0".repeat(64);
    }
    fn bytes(hash: &mut Sha256, data: &[u8]) {
        hash.update((data.len() as u64).to_be_bytes());
        hash.update(data);
    }
    fn visit(hash: &mut Sha256, data: &Data) {
        match data {
            Data::Text(v) => {
                hash.update(b"text");
                bytes(hash, v.as_bytes());
            }
            Data::Bytes(v) => {
                hash.update(b"bytes");
                bytes(hash, v);
            }
            Data::Int(v) => {
                hash.update(b"int");
                hash.update(v.to_be_bytes());
            }
            Data::Decimal(v) => {
                hash.update(b"decimal");
                hash.update(v.scale().to_be_bytes());
                bytes(hash, v.to_string().as_bytes());
            }
            Data::Bool(v) => {
                hash.update(b"bool");
                hash.update([u8::from(*v)]);
            }
            Data::Instant(v) => {
                hash.update(b"instant");
                bytes(hash, v.to_string().as_bytes());
            }
            Data::Duration(v) => {
                hash.update(b"duration");
                bytes(hash, v.to_string().as_bytes());
            }
            Data::Interval(v) => {
                hash.update(b"interval");
                bytes(hash, v.to_string().as_bytes());
            }
            Data::Option(None) => hash.update(b"none"),
            Data::Option(Some(v)) => {
                hash.update(b"some");
                visit(hash, v);
            }
            Data::List(values) => {
                hash.update(b"list");
                hash.update((values.len() as u64).to_be_bytes());
                for v in values {
                    visit(hash, v);
                }
            }
            Data::Record(values) => {
                hash.update(b"record");
                hash.update((values.len() as u64).to_be_bytes());
                let sorted: BTreeMap<_, _> = values.iter().collect();
                for (k, v) in sorted {
                    bytes(hash, k.as_bytes());
                    visit(hash, v);
                }
            }
            // Such inputs fail the common materialized contract check before installation.
            Data::Iter(_) => hash.update(b"unmaterialized"),
            Data::Dataset(reference) => {
                hash.update(b"dataset");
                for field in [
                    reference.store(),
                    reference.dataset(),
                    reference.manifest(),
                    reference.manifest_digest(),
                    reference.schema_digest(),
                ] {
                    bytes(hash, field.as_bytes());
                }
                for number in [
                    reference.generation(),
                    reference.manifest_bytes(),
                    reference.records(),
                    reference.authorization_generation(),
                ] {
                    hash.update(number.to_be_bytes());
                }
            }
        }
    }
    let mut hash = Sha256::new();
    hash.update(b"wes-view-input");
    visit(&mut hash, value.data());
    format!("{:x}", hash.finalize())
}

fn project(value: Value, path: &[String]) -> Option<Value> {
    if path.is_empty() {
        return Some(value);
    }
    crate::plan::project_value(&value, path).ok()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewRecord {
    pub id: String,
    pub value: Value,
}
impl ViewRecord {
    pub(crate) fn retire(&mut self, nodes: &BTreeSet<NodeId>) -> Result<(), InvalidRecord> {
        self.validate()?;
        let Data::List(entries) = self.value.data() else {
            return Err(InvalidRecord("invalid view checkpoint"));
        };
        let mut kept = Vec::new();
        for entry in entries {
            let id = NodeId::new(
                string(get(entry, "id").map_err(|_| InvalidRecord("view id"))?)
                    .map_err(|_| InvalidRecord("view id"))?,
            )
            .map_err(|_| InvalidRecord("view id"))?;
            if nodes.contains(&id) {
                continue;
            }
            let mut entry = entry.clone();
            if let Data::Record(fields) = &mut entry {
                let mut removed = false;
                if let Some(Data::List(input)) = fields.get_mut("input") {
                    if let [description] = input.as_slice() {
                        if let Ok(Data::Text(owner)) = get(description, "node") {
                            if nodes.iter().any(|node| node.as_str() == owner.as_ref()) {
                                *input = vec![record([
                                    ("kind", text("unlinked")),
                                    ("unavailable", Data::Bool(true)),
                                ])];
                                removed = true;
                            }
                        }
                    }
                }
                if let Some(Data::Record(slots)) = fields.get_mut("members") {
                    for data in slots.values_mut() {
                        if let Data::List(ids) = data {
                            let before = ids.len();
                            ids.retain(|v| !matches!(v,Data::Text(id) if nodes.iter().any(|n|n.as_str()==id.as_ref())));
                            removed |= before != ids.len();
                        }
                    }
                }
                if removed {
                    let revision =
                        number(&fields["revision"]).map_err(|_| InvalidRecord("view revision"))?;
                    fields.insert(
                        "revision".into(),
                        text(revision.saturating_add(1).to_string()),
                    );
                }
            }
            kept.push(entry);
        }
        self.value = Value::new(Shape::Unknown, Data::List(kept), Provenance::default())
            .map_err(|_| InvalidRecord("view checkpoint"))?;
        Ok(())
    }
    /// Only the last validated view checkpoint owns current retained input references.
    pub fn retained_inputs(
        &self,
    ) -> Result<Vec<(NodeId, crate::storage::ValueHandle)>, InvalidRecord> {
        self.validate()?;
        parse(&self.value)
            .map_err(|_| InvalidRecord("invalid view checkpoint"))?
            .into_iter()
            .filter_map(|entry| {
                let description = match list(&entry.input) {
                    Ok([description]) => description,
                    _ => return None,
                };
                if string(get(description, "kind").ok()?).ok()? != "retained" {
                    return None;
                }
                Some(
                    crate::storage::ValueHandle::new(
                        string(get(description, "handle").ok()?).ok()?,
                    )
                    .map(|handle| (entry.id, handle))
                    .map_err(|_| InvalidRecord("retained handle")),
                )
            })
            .collect()
    }
    pub fn validate(&self) -> Result<(), InvalidRecord> {
        if uuid::Uuid::parse_str(&self.id).is_err()
            || self.value.shape() != &Shape::Unknown
            || !self.value.data().is_storable_snapshot()
            || self.value.provenance().policy().is_private()
            || crate::value_size::value_charge(&self.value, 4 * 1024 * 1024).is_none()
        {
            return Err(InvalidRecord("invalid view checkpoint"));
        }
        parse(&self.value)
            .map(|_| ())
            .map_err(|_| InvalidRecord("invalid view checkpoint"))
    }
}
fn text(s: impl AsRef<str>) -> Data {
    Data::Text(s.as_ref().into())
}
fn record<const N: usize>(fields: [(&str, Data); N]) -> Data {
    Data::Record(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
}
fn strings(values: impl IntoIterator<Item = String>) -> Data {
    Data::List(values.into_iter().map(text).collect())
}
fn fields(data: &Data) -> Result<&indexmap::IndexMap<String, Data>, Error> {
    match data {
        Data::Record(m) => Ok(m),
        _ => Err(Error::Input),
    }
}
fn list(data: &Data) -> Result<&[Data], Error> {
    match data {
        Data::List(v) => Ok(v),
        _ => Err(Error::Input),
    }
}
fn string(data: &Data) -> Result<&str, Error> {
    match data {
        Data::Text(v) => Ok(v),
        _ => Err(Error::Input),
    }
}
fn number(data: &Data) -> Result<u64, Error> {
    string(data)?.parse().map_err(|_| Error::Input)
}
fn get<'a>(data: &'a Data, name: &str) -> Result<&'a Data, Error> {
    fields(data)?.get(name).ok_or(Error::Input)
}
fn map(data: &Data) -> Result<BTreeMap<String, Data>, Error> {
    Ok(fields(data)?
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect())
}
fn digest(data: &Data) -> Result<String, Error> {
    let digest = string(data)?;
    if digest.len() != 64 || !digest.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::Input);
    }
    Ok(digest.into())
}
fn port(data: &Data) -> Result<crate::graph::OutputPort, Error> {
    match string(data)? {
        "data" => Ok(crate::graph::OutputPort::Data),
        "error" => Ok(crate::graph::OutputPort::Error),
        "cancel" => Ok(crate::graph::OutputPort::Cancel),
        _ => Err(Error::Input),
    }
}
fn port_name(port: crate::graph::OutputPort) -> &'static str {
    match port {
        crate::graph::OutputPort::Data => "data",
        crate::graph::OutputPort::Error => "error",
        crate::graph::OutputPort::Cancel => "cancel",
    }
}
fn path(data: &Data) -> Result<Vec<String>, Error> {
    let path = list(data)?
        .iter()
        .map(|v| string(v).map(str::to_owned))
        .collect::<Result<Vec<_>, _>>()?;
    if path.len() > 64 {
        return Err(Error::Capacity);
    }
    Ok(path)
}
fn run(data: &Data) -> Result<Option<RunId>, Error> {
    let text = string(data)?;
    if text.is_empty() {
        Ok(None)
    } else {
        RunId::new(text).map(Some).map_err(|_| Error::Input)
    }
}
fn audit(data: &Data) -> Result<Option<SourceAudit>, Error> {
    match list(data)? {
        [] => Ok(None),
        [data] if fields(data)?.len() == 5 => Ok(Some(SourceAudit {
            node: NodeId::new(string(get(data, "node")?)?).map_err(|_| Error::Input)?,
            port: port(get(data, "port")?)?,
            fields: path(get(data, "fields")?)?,
            run: run(get(data, "run")?)?,
            digest: digest(get(data, "digest")?)?,
        })),
        _ => Err(Error::Input),
    }
}
fn encode_audit(origin: Option<&SourceAudit>) -> Data {
    Data::List(
        origin
            .into_iter()
            .map(|s| {
                record([
                    ("node", text(s.node.as_str())),
                    ("port", text(port_name(s.port))),
                    ("fields", strings(s.fields.clone())),
                    ("run", text(s.run.as_ref().map(RunId::as_str).unwrap_or(""))),
                    ("digest", text(&s.digest)),
                ])
            })
            .collect(),
    )
}
fn validate_input(data: &Data) -> Result<(), Error> {
    match list(data)? {
        [] => Ok(()),
        [data] => {
            let f = fields(data)?;
            match string(get(data, "kind")?)? {
                "unlinked" if f.len() == 2 => match (f.get("constant"), f.get("unavailable")) {
                    (Some(_), None) | (None, Some(Data::Bool(true))) => Ok(()),
                    _ => Err(Error::Input),
                },
                "current" if f.len() == 7 => {
                    NodeId::new(string(get(data, "node")?)?).map_err(|_| Error::Input)?;
                    port(get(data, "port")?)?;
                    path(get(data, "fields")?)?;
                    run(get(data, "run")?)?;
                    digest(get(data, "digest")?)?;
                    if !matches!(get(data, "valid")?, Data::Bool(_)) {
                        return Err(Error::Input);
                    }
                    Ok(())
                }
                "retained" if f.len() == 6 => {
                    NodeId::new(string(get(data, "node")?)?).map_err(|_| Error::Input)?;
                    if run(get(data, "run")?)?.is_none() {
                        return Err(Error::Input);
                    }
                    crate::storage::ValueHandle::new(string(get(data, "handle")?)?)
                        .map_err(|_| Error::Input)?;
                    digest(get(data, "digest")?)?;
                    audit(get(data, "origin")?)?;
                    Ok(())
                }
                _ => Err(Error::Input),
            }
        }
        _ => Err(Error::Input),
    }
}
#[derive(Clone)]
struct Saved {
    id: NodeId,
    identity: String,
    definition: String,
    digest: String,
    artifact: Option<String>,
    revision: u64,
    input: Data,
    members: BTreeMap<String, Vec<NodeId>>,
    bindings: BTreeMap<String, (NodeId, String)>,
    state: InteractionState,
    query: Option<SavedQuery>,
}

fn restored_package(
    catalogue: &wes_views::Catalogue,
    entry: &Saved,
) -> Result<Arc<Package>, Error> {
    let package = catalogue.get(&entry.definition).ok_or(Error::Definition)?;
    if package.digest != entry.digest {
        return Err(Error::Digest);
    }
    if entry.artifact != package.artifact {
        // Shipped renderer updates follow the application while the exact input,
        // interaction and slot contract stays unchanged. Installed author code
        // remains pinned to its checked artifact, never an automatic replacement.
        let current_builtin = wes_views::named(&entry.definition).is_some_and(|builtin| {
            builtin.digest == package.digest && builtin.artifact == package.artifact
        });
        if !current_builtin || entry.artifact.is_none() {
            return Err(Error::Definition);
        }
    }
    Ok(package.clone())
}
#[derive(Clone)]
struct SavedQuery {
    environment: Option<wes_core::environments::EnvironmentContext>,
    source: NodeId,
    port: String,
    template: String,
    revision: String,
    trigger: QueryTrigger,
    adapter: Option<QueryAdapter>,
}
fn encode_environment(context: Option<&wes_core::environments::EnvironmentContext>) -> Data {
    Data::List(
        context
            .into_iter()
            .map(|context| {
                record([
                    (
                        "selected",
                        Data::List(context.selected.iter().map(|name| text(name)).collect()),
                    ),
                    (
                        "revisions",
                        Data::Record(
                            context
                                .revisions
                                .iter()
                                .map(|(name, revision)| (name.clone(), text(&revision.to_string())))
                                .collect(),
                        ),
                    ),
                ])
            })
            .collect(),
    )
}
fn decode_environment(
    data: &Data,
) -> Result<Option<wes_core::environments::EnvironmentContext>, Error> {
    match list(data)? {
        [] => Ok(None),
        [value] if fields(value)?.len() == 2 => {
            let selected = match list(get(value, "selected")?)? {
                [] => None,
                [name] => Some(string(name)?.to_owned()),
                _ => return Err(Error::Input),
            };
            let revisions = fields(get(value, "revisions")?)?
                .iter()
                .map(|(name, revision)| {
                    Ok((
                        name.clone(),
                        string(revision)?.parse().map_err(|_| Error::Input)?,
                    ))
                })
                .collect::<Result<_, Error>>()?;
            let context = wes_core::environments::EnvironmentContext {
                selected,
                revisions,
            };
            context.validate().map_err(|_| Error::Input)?;
            Ok(Some(context))
        }
        _ => Err(Error::Input),
    }
}
fn parse(value: &Value) -> Result<Vec<Saved>, Error> {
    let entries = list(value.data())?;
    if entries.len() > wes_budgets::get("view.instances") as usize {
        return Err(Error::Capacity);
    }
    let mut seen = BTreeSet::new();
    let mut identities = BTreeSet::new();
    let mut edges = 0_usize;
    let mut links = 0_usize;
    let mut queries = 0_usize;
    entries
        .iter()
        .map(|entry| {
            if fields(entry)?.len() != 11 {
                return Err(Error::Input);
            }
            let id = NodeId::new(string(get(entry, "id")?)?).map_err(|_| Error::Input)?;
            let identity = string(get(entry, "identity")?)?.to_owned();
            if !seen.insert(id.clone())
                || !identities.insert(identity.clone())
                || uuid::Uuid::parse_str(&identity).is_err()
            {
                return Err(Error::Reference);
            }
            let mut members = BTreeMap::new();
            for (slot, ids) in fields(get(entry, "members")?)? {
                members.insert(
                    slot.clone(),
                    list(ids)?
                        .iter()
                        .map(|id| NodeId::new(string(id)?).map_err(|_| Error::Input))
                        .collect::<Result<_, _>>()?,
                );
            }
            let mut bindings = BTreeMap::new();
            for (field, binding) in fields(get(entry, "bindings")?)? {
                if fields(binding)?.len() != 2 {
                    return Err(Error::Input);
                }
                bindings.insert(
                    field.clone(),
                    (
                        NodeId::new(string(get(binding, "source")?)?).map_err(|_| Error::Input)?,
                        string(get(binding, "port")?)?.into(),
                    ),
                );
            }
            edges = edges.saturating_add(members.values().map(Vec::len).sum::<usize>());
            links = links.saturating_add(bindings.len());
            if edges > wes_budgets::get("view.edges") as usize
                || links > wes_budgets::get("view.bindings.total") as usize
                || bindings.len() > wes_budgets::get("view.bindings") as usize
            {
                return Err(Error::Capacity);
            }
            let state = get(entry, "state")?;
            if fields(state)?.len() != 3 {
                return Err(Error::Input);
            }
            let query = match list(get(entry, "query")?)? {
                [] => None,
                [query] if fields(query)?.len() == 7 => {
                    queries += 1;
                    if queries > wes_budgets::get("view.queries") as usize {
                        return Err(Error::Capacity);
                    }
                    let template = string(get(query, "template")?)?.to_string();
                    let port = string(get(query, "port")?)?.to_string();
                    let revision = string(get(query, "revision")?)?.to_string();
                    if !wes_language::binding_name(&template)
                        || !wes_language::binding_name(&port)
                        || revision.is_empty()
                        || revision.len() > 512
                    {
                        return Err(Error::Input);
                    }
                    Some(SavedQuery {
                        environment: decode_environment(get(query, "environment")?)?,
                        source: NodeId::new(string(get(query, "source")?)?)
                            .map_err(|_| Error::Input)?,
                        template,
                        port,
                        revision,
                        adapter: match list(get(query, "adapter")?)? {
                            [] => None,
                            [a] if fields(a)?.len() == 2 => {
                                let template = string(get(a, "template")?)?.to_string();
                                let revision = string(get(a, "revision")?)?.to_string();
                                if !wes_language::binding_name(&template)
                                    || revision.is_empty()
                                    || revision.len() > 512
                                {
                                    return Err(Error::Input);
                                }
                                Some(QueryAdapter { template, revision })
                            }
                            _ => return Err(Error::Input),
                        },
                        trigger: match string(get(query, "trigger")?)? {
                            "manual" => QueryTrigger::Manual,
                            "commit" => QueryTrigger::Commit,
                            _ => return Err(Error::Input),
                        },
                    })
                }
                _ => return Err(Error::Input),
            };
            validate_input(get(entry, "input")?)?;
            Ok(Saved {
                id,
                identity,
                definition: string(get(entry, "definition")?)?.into(),
                digest: string(get(entry, "digest")?)?.into(),
                artifact: match get(entry, "artifact")? {
                    Data::Option(None) => None,
                    Data::Option(Some(value)) => Some(string(value)?.into()),
                    _ => return Err(Error::Input),
                },
                revision: number(get(entry, "revision")?)?,
                input: get(entry, "input")?.clone(),
                members,
                bindings,
                query,
                state: InteractionState {
                    revision: number(get(state, "revision")?)?,
                    fields: map(get(state, "fields")?)?,
                    outputs: map(get(state, "outputs")?)?,
                },
            })
        })
        .collect()
}
impl Workspace {
    pub(crate) fn capture_views(&self) -> Result<ViewRecord, InvalidRecord> {
        let entries = self
            .views
            .instances
            .values()
            .map(|instance| {
                let s = &instance.snapshot;
                let input = match &s.input {
                    _ if s.query.is_some() => Data::List(vec![]),
                    None => Data::List(vec![]),
                    Some(input) => match &input.binding {
                        InputBinding::Unlinked => Data::List(vec![match input
                            .value
                            .as_ref()
                            .filter(|v| !v.provenance().policy().is_private())
                        {
                            Some(value) => record([
                                ("kind", text("unlinked")),
                                ("constant", value.data().clone()),
                            ]),
                            None => record([
                                ("kind", text("unlinked")),
                                ("unavailable", Data::Bool(true)),
                            ]),
                        }]),
                        InputBinding::Current(source) => {
                            let current = self.runtime().graph().node(&source.output.node);
                            let valid = current
                                .is_some_and(|n| source.matches(&source.output, &n.definition()))
                                && !self
                                    .runtime()
                                    .value_of(&source.output.node)
                                    .is_some_and(|v| v.provenance().policy().is_private());
                            Data::List(vec![record([
                                ("node", text(source.output.node.as_str())),
                                ("port", text(port_name(source.output.port))),
                                ("fields", strings(source.fields.clone())),
                                ("kind", text("current")),
                                (
                                    "run",
                                    text(source.run.as_ref().map(RunId::as_str).unwrap_or("")),
                                ),
                                ("valid", Data::Bool(valid)),
                                ("digest", text(&source.digest)),
                            ])])
                        }
                        InputBinding::Retained(reference) => Data::List(vec![record([
                            ("kind", text("retained")),
                            ("node", text(reference.node.as_str())),
                            ("run", text(reference.run.as_str())),
                            ("handle", text(reference.handle.as_str())),
                            ("digest", text(&reference.digest)),
                            ("origin", encode_audit(reference.origin.as_ref())),
                        ])]),
                    },
                };
                record([
                    ("id", text(s.id.as_str())),
                    ("identity", text(s.identity.as_ref())),
                    ("definition", text(&s.definition.manifest.name)),
                    ("digest", text(&s.definition.digest)),
                    (
                        "artifact",
                        Data::Option(s.definition.artifact.as_ref().map(|v| Box::new(text(v)))),
                    ),
                    ("revision", text(s.revision.to_string())),
                    ("input", input),
                    (
                        "query",
                        Data::List(
                            s.query
                                .iter()
                                .map(|q| {
                                    record([
                                        ("environment", encode_environment(q.environment.as_ref())),
                                        ("source", text(q.source.id.as_str())),
                                        ("port", text(&q.port)),
                                        ("template", text(&q.template)),
                                        ("revision", text(&q.template_revision)),
                                        (
                                            "adapter",
                                            Data::List(
                                                q.adapter
                                                    .iter()
                                                    .map(|a| {
                                                        record([
                                                            ("template", text(&a.template)),
                                                            ("revision", text(&a.revision)),
                                                        ])
                                                    })
                                                    .collect(),
                                            ),
                                        ),
                                        ("trigger", text(q.trigger.as_str())),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "members",
                        Data::Record(
                            s.members
                                .iter()
                                .map(|(k, ids)| {
                                    (k.clone(), strings(ids.iter().map(ToString::to_string)))
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "bindings",
                        Data::Record(
                            instance
                                .bindings
                                .iter()
                                .map(|(k, b)| {
                                    (
                                        k.clone(),
                                        record([
                                            ("source", text(b.source.id.as_str())),
                                            ("port", text(&b.port)),
                                        ]),
                                    )
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "state",
                        record([
                            ("revision", text(instance.interaction.revision.to_string())),
                            (
                                "fields",
                                Data::Record(
                                    instance.interaction.fields.clone().into_iter().collect(),
                                ),
                            ),
                            (
                                "outputs",
                                Data::Record(
                                    instance.interaction.outputs.clone().into_iter().collect(),
                                ),
                            ),
                        ]),
                    ),
                ])
            })
            .collect();
        let record = ViewRecord {
            id: uuid::Uuid::new_v4().to_string(),
            value: Value::new(Shape::Unknown, Data::List(entries), Provenance::default())
                .map_err(|_| InvalidRecord("invalid view checkpoint"))?,
        };
        record.validate()?;
        Ok(record)
    }

    /// Called once by the restore coordinator after declarations and normal held values are loaded.
    pub(crate) async fn restore_views(
        &mut self,
        saved: &ViewRecord,
        position: usize,
        history: &HistoryImage,
        storage: Option<&SessionStorage>,
        changed: &indexmap::IndexMap<NodeId, usize>,
    ) -> Result<(), Error> {
        saved.validate().map_err(|_| Error::Input)?;
        let entries = parse(&saved.value)?;
        let mut store = Store::new(self.views.packages.values().cloned(), self.views.limits)?;
        store.install_catalogue(self.views.catalogue().clone());
        let mut handles = BTreeMap::new();
        let mut recovered = BTreeMap::<crate::storage::ValueHandle, Option<Value>>::new();
        let mut read_budget = store.limits.total_input_bytes;
        for entry in &entries {
            // Retired definitions are never resurrected by a checkpoint from an older prefix.
            let Some(node) = self.runtime().graph().node(&entry.id) else {
                continue;
            };
            if !matches!(node.payload(),crate::tasks::BoundTask::View(task) if task.command()==wes_language::vocabulary::MetaCommand::ViewCreate)
            {
                return Err(Error::Reference);
            }
            let package = restored_package(store.catalogue(), entry)?;
            let description = list(&entry.input)?;
            if description.len() > 1 {
                return Err(Error::Input);
            }
            let input = if let Some(description) = description.first() {
                match string(get(description, "kind")?)? {
                    "unlinked" => {
                        let value = fields(description)?
                            .get("constant")
                            .map(|data| {
                                Value::new(
                                    package.input().shape(),
                                    data.clone(),
                                    Provenance::default(),
                                )
                                .map_err(|_| Error::Input)
                            })
                            .transpose()?;
                        Some(Input {
                            binding: InputBinding::Unlinked,
                            value,
                        })
                    }
                    "current" => {
                        let node = NodeId::new(string(get(description, "node")?)?)
                            .map_err(|_| Error::Input)?;
                        let port = port(get(description, "port")?)?;
                        let path = path(get(description, "fields")?)?;
                        let valid = get(description, "valid")? == &Data::Bool(true)
                            && changed.get(&node).is_none_or(|at| *at <= position);
                        let definition = self
                            .runtime()
                            .graph()
                            .node(&node)
                            .filter(|_| valid)
                            .map(|n| n.definition())
                            .unwrap_or_else(|| Arc::new(()));
                        let mut source = Source::new(
                            OutputRef {
                                node: node.clone(),
                                port,
                            },
                            definition,
                        )
                        .with_fields(path.clone())
                        .with_run(run(get(description, "run")?)?);
                        source.digest = digest(get(description, "digest")?)?;
                        let value = if valid {
                            match port {
                                crate::graph::OutputPort::Data => {
                                    self.runtime().value_of(&node).cloned()
                                }
                                crate::graph::OutputPort::Error
                                | crate::graph::OutputPort::Cancel => match self
                                    .runtime()
                                    .output(&source.output)
                                {
                                    crate::runtime::OutputState::Available(value) => Some(value),
                                    _ => None,
                                },
                            }
                            .filter(|v| !v.provenance().policy().is_private())
                            .and_then(|v| project(v, &path))
                        } else {
                            None
                        };
                        if let Some(value) = &value {
                            source.digest = input_digest(value);
                            source.run = if port == crate::graph::OutputPort::Data {
                                self.runtime().value_run(&node)
                            } else {
                                self.runtime().run_of(&node)
                            }
                            .cloned();
                        }
                        Some(Input {
                            binding: InputBinding::Current(source),
                            value,
                        })
                    }
                    "retained" => {
                        let reference = RetainedReference {
                            node: NodeId::new(string(get(description, "node")?)?)
                                .map_err(|_| Error::Input)?,
                            run: run(get(description, "run")?)?.ok_or(Error::Input)?,
                            handle: crate::storage::ValueHandle::new(string(get(
                                description,
                                "handle",
                            )?)?)
                            .map_err(|_| Error::Input)?,
                            digest: digest(get(description, "digest")?)?,
                            origin: audit(get(description, "origin")?)?,
                        };
                        let acknowledged = self.runtime().graph().node(&reference.node).is_some()
                            && history
                                .journal()
                                .iter()
                                .filter_map(|e| e.retained_result())
                                .any(|r| {
                                    r.node == reference.node
                                        && r.run == reference.run
                                        && r.handle == reference.handle
                                        && r.retention == crate::storage::Retention::Protected
                                });
                        if acknowledged && let Some(storage) = storage {
                            if !recovered.contains_key(&reference.handle)
                                && recovered.len() < 128
                                && read_budget > 0
                            {
                                let loaded = storage
                                    .worker
                                    .recover(reference.handle.clone())
                                    .await
                                    .ok()
                                    .flatten()
                                    .filter(|v| v.retention == crate::storage::Retention::Protected)
                                    .map(|v| v.loaded.value)
                                    .filter(|v| !v.provenance().policy().is_private())
                                    .filter(|v| {
                                        if let Some(charge) =
                                            crate::value_size::value_charge(v, read_budget)
                                        {
                                            read_budget -= charge;
                                            true
                                        } else {
                                            read_budget = 0;
                                            false
                                        }
                                    });
                                recovered.insert(reference.handle.clone(), loaded);
                            }
                        }
                        let value = acknowledged
                            .then(|| recovered.get(&reference.handle).and_then(Clone::clone))
                            .flatten()
                            .filter(|v| input_digest(v) == reference.digest);
                        Some(Input {
                            binding: InputBinding::Retained(reference),
                            value,
                        })
                    }
                    _ => return Err(Error::Input),
                }
            } else {
                None
            };
            store.create(entry.id.clone(), &entry.definition, &entry.digest, input)?;
            let instance = store
                .instances
                .get_mut(&entry.id)
                .expect("created instance");
            instance.input_observation_requested = false;
            instance.snapshot.observing = false;
            instance.handle.identity = entry.identity.as_str().into();
            instance.snapshot.identity = instance.handle.identity.clone();
            if instance
                .snapshot
                .input
                .as_ref()
                .is_some_and(|i| i.value.is_none())
            {
                instance.snapshot.input_problem = Some(
                    "Saved input is unavailable; bind another result or explicitly run the source"
                        .into(),
                );
            }
            handles.insert(entry.id.clone(), instance.handle.clone());
        }
        for entry in &entries {
            let Some(parent) = handles.get(&entry.id) else {
                continue;
            };
            for (slot, members) in &entry.members {
                for member in members {
                    let Some(child) = handles.get(member) else {
                        continue;
                    };
                    let revision = store.read(parent)?.revision;
                    store.connect(child, parent, Some(slot), revision)?;
                }
            }
        }
        for entry in &entries {
            let Some(target) = handles.get(&entry.id) else {
                continue;
            };
            for (field, (source, port)) in &entry.bindings {
                let Some(source_handle) = handles.get(source) else {
                    let contract = store.instances[&entry.id].snapshot.definition.input();
                    if !matches!(contract.kind(),wes_core::contracts::ContractKind::Record(fields) if fields.contains_key(field))
                    {
                        return Err(Error::Input);
                    }
                    let missing = Handle {
                        id: source.clone(),
                        owner: store.owner.clone(),
                        generation: Arc::new(()),
                        token: uuid::Uuid::new_v4().to_string().into(),
                        identity: uuid::Uuid::new_v4().to_string().into(),
                    };
                    let instance = store.instances.get_mut(&entry.id).expect("restored target");
                    instance.bindings.insert(
                        field.clone(),
                        bindings::Binding {
                            source: missing,
                            port: port.clone(),
                        },
                    );
                    instance.snapshot.linked_inputs = instance.bindings.keys().cloned().collect();
                    continue;
                };
                let revision = store.read(target)?.revision;
                store.link(source_handle, port, target, field, revision)?;
            }
            store
                .instances
                .get_mut(&entry.id)
                .expect("restored instance")
                .snapshot
                .revision = entry.revision;
        }
        for entry in &entries {
            let (Some(target), Some(saved)) = (handles.get(&entry.id), &entry.query) else {
                continue;
            };
            let source = handles
                .get(&saved.source)
                .cloned()
                .unwrap_or_else(|| Handle {
                    id: saved.source.clone(),
                    owner: store.owner.clone(),
                    generation: Arc::new(()),
                    token: uuid::Uuid::new_v4().to_string().into(),
                    identity: uuid::Uuid::new_v4().to_string().into(),
                });
            let binding = QueryBinding {
                environment: saved.environment.clone(),
                source: source.clone(),
                port: saved.port.clone(),
                template: saved.template.clone(),
                template_revision: saved.revision.clone(),
                adapter: saved.adapter.clone(),
                trigger: saved.trigger,
            };
            if let Ok(from) = store.read(&source) {
                self.validate_query_contracts(
                    &binding,
                    &from.definition,
                    &store.read(target)?.definition,
                )
                .map_err(|_| Error::Incompatible)?;
                let revision = store.read(target)?.revision;
                store.configure_query(target, binding, revision)?;
            } else {
                let i = store.instances.get_mut(&entry.id).expect("restored target");
                i.snapshot.query = Some(binding);
                i.snapshot.input_problem =
                    Some("Query source is unavailable; configure its binding again".into());
            }
            store
                .instances
                .get_mut(&entry.id)
                .expect("restored query")
                .snapshot
                .revision = entry.revision;
        }
        // Only the coordinator owns shared state; child snapshots must remain empty.
        for entry in &entries {
            let Some(handle) = handles.get(&entry.id) else {
                continue;
            };
            if entry.state.revision == 0 {
                if !entry.state.fields.is_empty() || !entry.state.outputs.is_empty() {
                    return Err(Error::Interaction);
                }
                continue;
            }
            let owner = store.interaction_owner(handle)?;
            if owner.id != entry.id {
                return Err(Error::Coordinator);
            }
            store.commit_interaction(
                handle,
                InteractionEdit {
                    events: vec![],
                    owner: entry.id.clone(),
                    identity: entry.identity.clone(),
                    definition_revision: entry.revision,
                    revision: 0,
                    fields: entry.state.fields.clone(),
                    outputs: entry.state.outputs.clone(),
                },
            )?;
            store
                .instances
                .get_mut(&entry.id)
                .expect("restored instance")
                .interaction
                .revision = entry.state.revision;
        }
        // Fresh process authority comes only from this validated workspace-owned definition.
        for (node, handle) in &handles {
            let value = store.value(handle)?;
            let run = self.runtime().run_of(node).cloned();
            self.restore_view_reference(node, crate::runtime::RestoredState::Ready(value), run)
                .map_err(|_| Error::Reference)?;
        }
        let references = self
            .runtime()
            .graph()
            .nodes()
            .filter_map(|node| {
                let value = self.runtime().value_of(node.id())?;
                if value.shape() != &Shape::Meta(wes_core::MetaType::ViewInstance) {
                    return None;
                }
                let data = fields(value.data()).ok()?;
                let id = NodeId::new(string(data.get("id")?).ok()?).ok()?;
                let instance = store.instances.get(&id)?;
                let renewed = store.value(&instance.handle).ok()?;
                (renewed.data() == value.data()).then(|| {
                    (
                        node.id().clone(),
                        renewed,
                        self.runtime().run_of(node.id()).cloned(),
                    )
                })
            })
            .collect::<Vec<_>>();
        for (node, value, run) in references {
            self.restore_view_reference(&node, crate::runtime::RestoredState::Ready(value), run)
                .map_err(|_| Error::Reference)?;
        }
        self.views = store;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shipped_renderer_updates_require_the_same_contract_and_custom_code_stays_pinned() {
        let mut workspace = Workspace::local(crate::providers::LocalScope::new("fixture").unwrap());
        let metric = wes_views::named("Metric").unwrap();
        workspace
            .views
            .create(NodeId::new("card").unwrap(), "Metric", &metric.digest, None)
            .unwrap();
        let mut entry = parse(&workspace.capture_views().unwrap().value)
            .unwrap()
            .remove(0);
        entry.artifact = Some("a".repeat(64));
        let current = restored_package(workspace.views.catalogue(), &entry).unwrap();
        assert_eq!(current.artifact, metric.artifact);
        let mut changed = entry.clone();
        changed.digest = "b".repeat(64);
        assert!(matches!(
            restored_package(workspace.views.catalogue(), &changed),
            Err(Error::Digest)
        ));
        changed = entry.clone();
        changed.artifact = None;
        assert!(matches!(
            restored_package(workspace.views.catalogue(), &changed),
            Err(Error::Definition)
        ));

        let mut custom = Package::parse(r#"{"name":"Notes","id":"notes","summary":"Plain notes","renderer":"View.tsx","input":"Note","outputs":{},"slots":{}}"#, "types: {Note: {base: Record, fields: {title: Text}}}").unwrap();
        custom.artifact = Some("c".repeat(64));
        entry.definition = custom.manifest.name.clone();
        entry.digest = custom.digest.clone();
        let catalogue = wes_views::Catalogue::new([Arc::new(custom)]).unwrap();
        assert!(matches!(
            restored_package(&catalogue, &entry),
            Err(Error::Definition)
        ));
        entry.artifact = Some("c".repeat(64));
        assert!(restored_package(&catalogue, &entry).is_ok());
    }
    #[test]
    fn query_environment_checkpoint_is_an_acknowledgement_not_an_execution_grant() {
        let context = wes_core::environments::EnvironmentContext {
            selected: Some("synthetic".into()),
            revisions: [(
                "synthetic".into(),
                wes_core::environments::Revision::evidence("fixture", ["definition"]),
            )]
            .into(),
        };
        let encoded = encode_environment(Some(&context));
        assert_eq!(decode_environment(&encoded).unwrap(), Some(context));
        assert_eq!(decode_environment(&encode_environment(None)).unwrap(), None);
        assert!(
            decode_environment(&Data::List(vec![record([
                ("selected", Data::List(vec![text("missing")])),
                ("revisions", Data::Record(Default::default())),
            ])]))
            .is_err()
        );
    }
    #[test]
    fn checkpoint_omits_private_payloads_and_retirement_prunes_owned_view_state() {
        let mut workspace = Workspace::local(crate::providers::LocalScope::new("fixture").unwrap());
        let card = wes_views::named("Metric").unwrap();
        let private = Value::new(
            card.input().shape(),
            record([("view", text("metric")), ("value", Data::Int(987654321))]),
            Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
        )
        .unwrap();
        let id = NodeId::new("card").unwrap();
        let child = workspace
            .views
            .create(
                id.clone(),
                "Metric",
                &card.digest,
                Some(Input::constant(private)),
            )
            .unwrap();
        let dashboard = wes_views::named("Dashboard").unwrap();
        let parent = workspace
            .views
            .create(
                NodeId::new("board").unwrap(),
                "Dashboard",
                &dashboard.digest,
                None,
            )
            .unwrap();
        workspace.views.connect(&child, &parent, None, 0).unwrap();
        let mut saved = workspace.capture_views().unwrap();
        assert!(!format!("{:?}", saved.value.data()).contains("987654321"));
        assert!(format!("{:?}", saved.value.data()).contains("unavailable"));
        saved.retire(&[id].into()).unwrap();
        let parsed = parse(&saved.value).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].members["members"].is_empty());
        assert_eq!(parsed[0].revision, 2);
    }
}
