//! Workspace-owned view definitions and finite bindings. No provider execution lives here.
//!
//! Handles are process-local capabilities. Wire metadata cannot construct one. A source binding
//! retains an immutable Value (shared storage), independently of membership and presentation mounts.
use crate::graph::{NodeId, OutputRef};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use wes_core::{Data, Value};
mod bindings;
pub mod commands;
mod evidence;
pub(crate) use persistence::input_digest as query_input_digest;
mod observations;
mod queries;
pub use queries::{QueryAdapter, QueryBinding, QueryTrigger};
mod persistence;
pub use observations::MountAction;
mod references;
pub use persistence::ViewRecord;
pub(crate) use references::PinIntent;
pub use references::{InputBinding, InputDelivery, RetainedReference, SourceAudit};
mod events;
mod interaction;
pub use bindings::InputPatches;
pub use events::EventEmission;
pub use interaction::{InteractionEdit, InteractionState};
use wes_views::Package;

#[derive(Clone)]
pub struct Handle {
    id: NodeId,
    owner: Arc<()>,
    generation: Arc<()>,
    token: Arc<str>,
    identity: Arc<str>,
}
impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewHandle")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
impl Handle {
    pub fn id(&self) -> &NodeId {
        &self.id
    }
}

/// Identity of the producing definition, not a user alias or a run revision.
#[derive(Clone, Debug)]
pub struct Source {
    pub output: OutputRef,
    definition: Arc<()>,
    pub fields: Vec<String>,
    pub(crate) run: Option<crate::runtime::RunId>,
    pub(crate) digest: String,
}
impl Source {
    /// Execution that produced the captured input, independent of a later pending attempt.
    pub fn run(&self) -> Option<&crate::runtime::RunId> {
        self.run.as_ref()
    }
    pub(crate) fn new(output: OutputRef, definition: Arc<()>) -> Self {
        Self {
            output,
            definition,
            fields: vec![],
            run: None,
            digest: String::new(),
        }
    }
    pub(crate) fn with_digest(mut self, value: &Value) -> Self {
        self.digest = persistence::input_digest(value);
        self
    }
    pub(crate) fn with_run(mut self, run: Option<crate::runtime::RunId>) -> Self {
        self.run = run;
        self
    }
    pub(crate) fn with_fields(mut self, fields: Vec<String>) -> Self {
        self.fields = fields;
        self
    }
    pub(crate) fn matches(&self, output: &OutputRef, definition: &Arc<()>) -> bool {
        self.output == *output && Arc::ptr_eq(&self.definition, definition)
    }
}

#[derive(Clone, Debug)]
pub struct Input {
    pub binding: InputBinding,
    value: Option<Value>,
}
impl Input {
    pub fn constant(value: Value) -> Self {
        Self {
            binding: InputBinding::Unlinked,
            value: Some(value),
        }
    }
    pub(crate) fn from_source(source: Source, value: Value) -> Self {
        Self {
            binding: InputBinding::Current(source),
            value: Some(value),
        }
    }
    pub fn source(&self) -> Option<&Source> {
        match &self.binding {
            InputBinding::Current(source) => Some(source),
            _ => None,
        }
    }
    pub(crate) fn source_mut(&mut self) -> Option<&mut Source> {
        match &mut self.binding {
            InputBinding::Current(source) => Some(source),
            _ => None,
        }
    }
    pub fn value(&self) -> Option<&Value> {
        self.value.as_ref()
    }
}

/// View interaction and definition checkpoints are public channels. Encrypted
/// result storage does not grant permission to copy its contents into them.
pub(crate) fn public_input(value: &Value) -> bool {
    let policy = value.provenance().policy();
    !policy.is_confidential() && !policy.is_unknown()
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub instances: usize,
    pub edges: usize,
    pub input_bytes: u64,
    pub total_input_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            instances: wes_budgets::get("view.instances") as usize,
            edges: wes_budgets::get("view.edges") as usize,
            input_bytes: wes_budgets::get("view.input.bytes") as u64,
            total_input_bytes: wes_budgets::get("view.inputs.bytes") as u64,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("View reference does not belong to this workspace or no longer exists")]
    Reference,
    #[error(
        "View display session expired or closed; reopen the view. Source commands were not rerun."
    )]
    DisplayClosed,
    #[error("View definition is not installed")]
    Definition,
    #[error("View definition changed; rebuild the view with the current contract")]
    Digest,
    #[error("View changed since this edit was prepared; read its current revision")]
    Revision,
    #[error("A view already owns this identity")]
    Duplicate,
    #[error("View input is not a valid finite binding")]
    Input,
    #[error("View input has type Unknown; use :type check before binding")]
    InputUnknown,
    #[error("View input does not satisfy {contract}: {}", input_issue_text(.issues))]
    InputContract {
        contract: String,
        issues: Vec<wes_core::ValidationIssue>,
    },
    #[error("View capacity exceeded")]
    Capacity,
    #[error("Select a declared view slot; no default slot is available")]
    Slot,
    #[error("View slot does not accept this definition or interaction protocol")]
    Incompatible,
    #[error("View slot is full")]
    Full,
    #[error("View membership would form a cycle")]
    Cycle,
    #[error("View already belongs to an interaction coordinator")]
    Coordinator,
    #[error("View is not a member of this slot")]
    NotMember,
    #[error("View interaction does not satisfy its declared shared contract")]
    Interaction,
}

fn input_issue_text(issues: &[wes_core::ValidationIssue]) -> String {
    issues
        .iter()
        .map(|issue| {
            let path = if issue.path.is_empty() {
                "/"
            } else {
                &issue.path
            };
            format!("{path}: {}", issue.message)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub id: NodeId,
    pub identity: Arc<str>,
    pub definition: Arc<Package>,
    pub revision: u64,
    pub input_revision: u64,
    pub input_delivery: InputDelivery,
    pub observing: bool,
    pub query: Option<QueryBinding>,
    pub query_running: bool,
    pub input_problem: Option<String>,
    pub input: Option<Input>,
    pub members: BTreeMap<String, Vec<NodeId>>,
    pub linked_inputs: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct Frame {
    pub root: NodeId,
    pub instances: Vec<Snapshot>,
}
impl Frame {
    pub fn binding_revisions(&self) -> Vec<(NodeId, Arc<str>, u64)> {
        self.instances
            .iter()
            .map(|i| (i.id.clone(), i.identity.clone(), i.revision))
            .collect()
    }
    pub fn revisions(&self) -> Vec<(NodeId, u64, u64)> {
        self.instances
            .iter()
            .map(|i| (i.id.clone(), i.revision, i.input_revision))
            .collect()
    }
}
#[derive(Debug)]
struct Instance {
    handle: Handle,
    snapshot: Snapshot,
    charge: u64,
    interaction: InteractionState,
    events: events::Capture,
    bindings: BTreeMap<String, bindings::Binding>,
    query_runtime: queries::QueryRuntime,
    /// Read intent survives a presentation lease; it never grants query execution authority.
    input_observation_requested: bool,
}

/// All edits validate before changing state. A workspace owner serializes access to this store.
#[derive(Debug)]
pub struct Store {
    command_epoch: uuid::Uuid,
    owner: Arc<()>,
    packages: wes_views::Catalogue,
    instances: BTreeMap<NodeId, Instance>,
    limits: Limits,
    charged: u64,
    edges: usize,
    mounts: BTreeMap<String, observations::Mount>,
    visible: BTreeSet<NodeId>,
}
impl Default for Store {
    fn default() -> Self {
        let mut store = Self::new([], Limits::default()).expect("empty view catalogue");
        store.packages = wes_views::Catalogue::default();
        store
    }
}
impl Store {
    pub fn new(
        packages: impl IntoIterator<Item = Arc<Package>>,
        limits: Limits,
    ) -> Result<Self, Error> {
        let definitions = wes_views::Catalogue::new(packages).map_err(|_| Error::Incompatible)?;
        Ok(Self {
            command_epoch: uuid::Uuid::new_v4(),
            owner: Arc::new(()),
            packages: definitions,
            instances: BTreeMap::new(),
            limits,
            charged: 0,
            edges: 0,
            mounts: BTreeMap::new(),
            visible: BTreeSet::new(),
        })
    }
    pub fn catalogue(&self) -> &wes_views::Catalogue {
        &self.packages
    }
    pub(crate) fn install_catalogue(&mut self, catalogue: wes_views::Catalogue) {
        self.packages = catalogue;
    }
    pub fn create(
        &mut self,
        id: NodeId,
        definition: &str,
        digest: &str,
        input: Option<Input>,
    ) -> Result<Handle, Error> {
        if self.instances.contains_key(&id) {
            return Err(Error::Duplicate);
        }
        if self.instances.len() >= self.limits.instances {
            return Err(Error::Capacity);
        }
        let package = self.packages.get(definition).ok_or(Error::Definition)?;
        if package.digest != digest {
            return Err(Error::Digest);
        }
        let charge = self.check_input(package, input.as_ref(), 0)?;
        let handle = Handle {
            id: id.clone(),
            owner: self.owner.clone(),
            generation: Arc::new(()),
            token: uuid::Uuid::new_v4().to_string().into(),
            identity: uuid::Uuid::new_v4().to_string().into(),
        };
        let input_observation_requested =
            input.as_ref().is_some_and(|input| input.binding.current());
        let snapshot = Snapshot {
            id: id.clone(),
            identity: handle.identity.clone(),
            definition: package.clone(),
            revision: 0,
            input_revision: 0,
            input_delivery: InputDelivery::Finite,
            observing: false,
            query: None,
            query_running: false,
            input_problem: None,
            input,
            members: BTreeMap::new(),
            linked_inputs: vec![],
        };
        self.instances.insert(
            id,
            Instance {
                handle: handle.clone(),
                snapshot,
                charge,
                interaction: InteractionState::default(),
                events: events::Capture::default(),
                bindings: BTreeMap::new(),
                query_runtime: queries::QueryRuntime::default(),
                input_observation_requested,
            },
        );
        self.charged += charge;
        Ok(handle)
    }
    pub(crate) fn contains(&self, node: &NodeId) -> bool {
        self.instances.contains_key(node)
    }
    pub fn read(&self, handle: &Handle) -> Result<Snapshot, Error> {
        Ok(self.instance(handle)?.snapshot.clone())
    }
    pub(crate) fn resolve(&self, value: &Value) -> Result<Handle, Error> {
        if value.shape() != &wes_core::Shape::Meta(wes_core::MetaType::ViewInstance) {
            return Err(Error::Reference);
        }
        let token = value.management_authority().ok_or(Error::Reference)?;
        self.instances
            .values()
            .find(|i| i.handle.token.as_ref() == token)
            .map(|i| i.handle.clone())
            .ok_or(Error::Reference)
    }
    pub(crate) fn value(&self, handle: &Handle) -> Result<Value, Error> {
        let instance = self.instance(handle)?;
        let snapshot = &instance.snapshot;
        Ok(Value::management(
            wes_core::MetaType::ViewInstance,
            wes_core::Data::Record(
                [
                    (
                        "id".into(),
                        wes_core::Data::Text(snapshot.id.as_str().into()),
                    ),
                    (
                        "instance".into(),
                        wes_core::Data::Text(snapshot.identity.clone()),
                    ),
                    (
                        "definition".into(),
                        wes_core::Data::Text(snapshot.definition.manifest.name.as_str().into()),
                    ),
                    (
                        "digest".into(),
                        wes_core::Data::Text(snapshot.definition.digest.as_str().into()),
                    ),
                ]
                .into_iter()
                .collect(),
            ),
            handle.token.to_string(),
        ))
    }
    pub(crate) fn frame(&self, handle: &Handle) -> Result<Frame, Error> {
        self.instance(handle)?;
        let mut pending = vec![handle.id.clone()];
        let mut seen = BTreeSet::new();
        let mut instances = vec![];
        let mut bytes = 0_u64;
        while let Some(id) = pending.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let instance = self.instances.get(&id).ok_or(Error::Reference)?;
            bytes = bytes.checked_add(instance.charge).ok_or(Error::Capacity)?;
            if instances.len() >= wes_budgets::get("ui.view.frames") as usize
                || bytes > self.limits.input_bytes
            {
                return Err(Error::Capacity);
            }
            pending.extend(instance.snapshot.members.values().flatten().cloned());
            instances.push(instance.snapshot.clone());
        }
        Ok(Frame {
            root: handle.id.clone(),
            instances,
        })
    }
    pub(crate) fn inspect(&self, value: &Value) -> Result<wes_core::Data, Error> {
        use wes_core::Data;
        let instance = self.read(&self.resolve(value)?)?;
        Ok(Data::Record(
            [
                ("id".into(), Data::Text(instance.id.as_str().into())),
                (
                    "definition".into(),
                    Data::Text(instance.definition.manifest.name.as_str().into()),
                ),
                (
                    "revision".into(),
                    Data::Text(instance.revision.to_string().into()),
                ),
                ("inputBound".into(), Data::Bool(instance.input.is_some())),
                (
                    "inputAvailable".into(),
                    Data::Bool(instance.input.as_ref().and_then(Input::value).is_some()),
                ),
                (
                    "inputReference".into(),
                    Data::Text(
                        instance
                            .input
                            .as_ref()
                            .map(|i| i.binding.kind())
                            .unwrap_or("unlinked")
                            .into(),
                    ),
                ),
                (
                    "query".into(),
                    Data::List(
                        instance
                            .query
                            .iter()
                            .map(|q| {
                                Data::Record(
                                    [
                                        ("template".into(), Data::Text(q.template.clone().into())),
                                        (
                                            "environment".into(),
                                            Data::Text(
                                                q.environment
                                                    .as_ref()
                                                    .and_then(|c| c.selected.as_deref())
                                                    .unwrap_or("")
                                                    .into(),
                                            ),
                                        ),
                                        (
                                            "mode".into(),
                                            Data::Text(
                                                if q.adapter.is_some() {
                                                    "live"
                                                } else {
                                                    "finite"
                                                }
                                                .into(),
                                            ),
                                        ),
                                        (
                                            "adapter".into(),
                                            Data::Text(
                                                q.adapter
                                                    .as_ref()
                                                    .map(|a| a.template.as_str())
                                                    .unwrap_or("")
                                                    .into(),
                                            ),
                                        ),
                                        ("source".into(), Data::Text(q.source.id.as_str().into())),
                                        ("output".into(), Data::Text(q.port.clone().into())),
                                        ("trigger".into(), Data::Text(q.trigger.as_str().into())),
                                        ("running".into(), Data::Bool(instance.query_running)),
                                    ]
                                    .into_iter()
                                    .collect(),
                                )
                            })
                            .collect(),
                    ),
                ),
                ("observing".into(), Data::Bool(instance.observing)),
                (
                    "inputBindings".into(),
                    Data::Record(
                        self.instances[&instance.id]
                            .bindings
                            .iter()
                            .map(|(field, b)| {
                                (
                                    field.clone(),
                                    Data::Record(
                                        [
                                            (
                                                "source".into(),
                                                Data::Text(b.source.id.as_str().into()),
                                            ),
                                            ("output".into(), Data::Text(b.port.as_str().into())),
                                        ]
                                        .into(),
                                    ),
                                )
                            })
                            .collect(),
                    ),
                ),
                (
                    "members".into(),
                    Data::Record(
                        instance
                            .members
                            .into_iter()
                            .map(|(k, v)| {
                                (
                                    k,
                                    Data::List(
                                        v.into_iter()
                                            .map(|id| Data::Text(id.as_str().into()))
                                            .collect(),
                                    ),
                                )
                            })
                            .collect(),
                    ),
                ),
            ]
            .into_iter()
            .collect(),
        ))
    }
    pub fn bind(
        &mut self,
        handle: &Handle,
        revision: u64,
        input: Option<Input>,
    ) -> Result<u64, Error> {
        let instance = self.editable(handle, revision)?;
        let charge = self.check_input(
            &instance.snapshot.definition,
            input.as_ref(),
            instance.charge,
        )?;
        let query_runtime = instance.query_runtime.invalidated()?;
        let old_charge = instance.charge;
        let instance = self
            .instances
            .get_mut(&handle.id)
            .expect("checked instance");
        let requested = input.as_ref().is_some_and(|input| input.binding.current());
        instance.snapshot.input = input;
        instance.snapshot.query = None;
        instance.snapshot.query_running = false;
        instance.query_runtime = query_runtime;
        instance.snapshot.observing = requested && self.visible.contains(&handle.id);
        instance.input_observation_requested = requested;
        instance.snapshot.input_problem = None;
        instance.snapshot.input_revision = 0;
        instance.snapshot.revision += 1;
        instance.charge = charge;
        self.charged = self.charged - old_charge + charge;
        Ok(instance.snapshot.revision)
    }
    pub fn connect(
        &mut self,
        child: &Handle,
        parent: &Handle,
        slot: Option<&str>,
        revision: u64,
    ) -> Result<u64, Error> {
        let child_instance = self.instance(child)?;
        let parent_instance = self.editable(parent, revision)?;
        let (name, contract) = Self::slot(&parent_instance.snapshot.definition, slot)?;
        let child_package = &child_instance.snapshot.definition;
        if (!contract.accepts.is_empty()
            && !contract.accepts.contains(&child_package.manifest.name))
            || contract.protocol.as_ref().is_some_and(|p| {
                child_package
                    .manifest
                    .interaction
                    .as_ref()
                    .is_none_or(|i| &i.protocol != p)
            })
        {
            return Err(Error::Incompatible);
        }
        let members = parent_instance.snapshot.members.get(name);
        if members.is_some_and(|m| m.contains(&child.id)) {
            return Ok(revision);
        }
        if members.is_some_and(|m| m.len() >= contract.max) {
            return Err(Error::Full);
        }
        if self.edges >= self.limits.edges {
            return Err(Error::Capacity);
        }
        if self.reaches(&child.id, &parent.id)
            || self
                .instances
                .keys()
                .filter(|id| self.reaches(id, &parent.id))
                .any(|ancestor| {
                    self.instances
                        .keys()
                        .filter(|id| self.reaches(&child.id, id))
                        .any(|source| self.binding_reaches(source, ancestor))
                })
        {
            return Err(Error::Cycle);
        }
        if contract.coordinates
            && self.instances.values().any(|i| {
                i.snapshot.members.iter().any(|(s, m)| {
                    m.contains(&child.id) && i.snapshot.definition.manifest.slots[s].coordinates
                })
            })
        {
            return Err(Error::Coordinator);
        }
        let coordinates = contract.coordinates;
        let name = name.to_owned();
        if coordinates {
            // The coordinator becomes the sole owner. Inactive standalone state
            // must not survive on the member or enter a workspace checkpoint.
            let child = self.instances.get_mut(&child.id).expect("checked instance");
            child.interaction = InteractionState::default();
            child.events = events::Capture::default();
            // Invalidate standalone edits prepared before the ownership transfer.
            child.snapshot.revision += 1;
        }
        let instance = self
            .instances
            .get_mut(&parent.id)
            .expect("checked instance");
        instance
            .snapshot
            .members
            .entry(name)
            .or_default()
            .push(child.id.clone());
        instance.snapshot.revision += 1;
        self.edges += 1;
        let revision = instance.snapshot.revision;
        self.reconcile_mounts();
        Ok(revision)
    }
    pub fn disconnect(
        &mut self,
        child: &Handle,
        parent: &Handle,
        slot: Option<&str>,
        revision: u64,
    ) -> Result<u64, Error> {
        self.instance(child)?;
        let instance = self.editable(parent, revision)?;
        let (name, _) = Self::slot(&instance.snapshot.definition, slot)?;
        if !instance
            .snapshot
            .members
            .get(name)
            .is_some_and(|m| m.contains(&child.id))
        {
            return Err(Error::NotMember);
        }
        let name = name.to_owned();
        let instance = self
            .instances
            .get_mut(&parent.id)
            .expect("checked instance");
        instance
            .snapshot
            .members
            .get_mut(&name)
            .expect("checked slot")
            .retain(|id| id != &child.id);
        instance.snapshot.revision += 1;
        self.edges -= 1;
        let revision = instance.snapshot.revision;
        self.reconcile_mounts();
        Ok(revision)
    }
    pub fn remove(&mut self, handle: &Handle, revision: u64) -> Result<(), Error> {
        self.editable(handle, revision)?;
        // Removing an instance also edits its parents. Check all revision counters first.
        if self.instances.values().any(|i| {
            i.snapshot.revision == u64::MAX
                && i.snapshot.members.values().any(|m| m.contains(&handle.id))
        }) {
            return Err(Error::Capacity);
        }
        self.retire_node(&handle.id);
        Ok(())
    }
    fn check_input(
        &self,
        package: &Package,
        input: Option<&Input>,
        old_charge: u64,
    ) -> Result<u64, Error> {
        let Some(input) = input else {
            return Ok(0);
        };
        let Some(value) = input.value() else {
            return Ok(0);
        };
        let charge = crate::value_size::value_charge(value, self.limits.input_bytes)
            .ok_or(Error::Capacity)?;
        if value.shape() == &wes_core::Shape::Unknown {
            return Err(Error::InputUnknown);
        }
        if !value.data().is_storable_snapshot() || value.shape().contains_meta() {
            return Err(Error::Input);
        }
        package
            .validate_input(value.data())
            .map_err(|issues| Error::InputContract {
                contract: package.manifest.input.clone(),
                issues,
            })?;
        if self
            .charged
            .saturating_sub(old_charge)
            .checked_add(charge)
            .is_none_or(|n| n > self.limits.total_input_bytes)
        {
            return Err(Error::Capacity);
        }
        Ok(charge)
    }
    fn instance(&self, handle: &Handle) -> Result<&Instance, Error> {
        if !Arc::ptr_eq(&self.owner, &handle.owner) {
            return Err(Error::Reference);
        }
        self.instances
            .get(&handle.id)
            .filter(|i| Arc::ptr_eq(&i.handle.generation, &handle.generation))
            .ok_or(Error::Reference)
    }
    fn editable(&self, handle: &Handle, revision: u64) -> Result<&Instance, Error> {
        let instance = self.instance(handle)?;
        if instance.snapshot.revision != revision {
            return Err(Error::Revision);
        }
        if revision == u64::MAX {
            return Err(Error::Capacity);
        }
        Ok(instance)
    }
    fn slot<'a>(
        package: &'a Package,
        slot: Option<&str>,
    ) -> Result<(&'a str, &'a wes_views::Slot), Error> {
        package
            .manifest
            .slots
            .iter()
            .find(|(name, contract)| slot.map_or(contract.default, |s| name.as_str() == s))
            .map(|(name, slot)| (name.as_str(), slot))
            .ok_or(Error::Slot)
    }
    fn reaches(&self, from: &NodeId, to: &NodeId) -> bool {
        let mut pending = vec![from];
        let mut visited = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if id == to {
                return true;
            }
            if visited.insert(id)
                && let Some(instance) = self.instances.get(id)
            {
                pending.extend(instance.snapshot.members.values().flatten());
            }
        }
        false
    }
    pub(crate) fn affected(&self, handle: &Handle) -> Result<Vec<NodeId>, Error> {
        self.instance(handle)?;
        Ok(self
            .instances
            .keys()
            .filter(|id| self.reaches(id, &handle.id))
            .cloned()
            .collect())
    }
    pub(crate) fn affected_node(&self, node: &NodeId) -> Vec<NodeId> {
        let direct = self
            .instances
            .values()
            .filter(|i| {
                i.snapshot
                    .input
                    .as_ref()
                    .is_some_and(|input| match &input.binding {
                        InputBinding::Current(source) => &source.output.node == node,
                        InputBinding::Retained(saved) => &saved.node == node,
                        InputBinding::Unlinked => false,
                    })
            })
            .map(|i| i.handle.id.clone())
            .collect::<Vec<_>>();
        self.instances
            .keys()
            .filter(|id| {
                self.reaches(id, node) || direct.iter().any(|child| self.reaches(id, child))
            })
            .cloned()
            .collect()
    }
    pub(crate) fn retained_inputs(&self) -> Vec<(NodeId, crate::storage::ValueHandle)> {
        self.instances
            .values()
            .filter_map(
                |i| match i.snapshot.input.as_ref().map(|input| &input.binding) {
                    Some(InputBinding::Retained(saved)) => {
                        Some((i.handle.id.clone(), saved.handle.clone()))
                    }
                    _ => None,
                },
            )
            .collect()
    }
    pub(crate) fn retire_node(&mut self, node: &NodeId) {
        if let Some(instance) = self.instances.remove(node) {
            self.charged -= instance.charge;
            self.edges -= instance
                .snapshot
                .members
                .values()
                .map(Vec::len)
                .sum::<usize>();
        }
        for instance in self.instances.values_mut() {
            let lost = instance
                .snapshot
                .input
                .as_ref()
                .is_some_and(|input| match &input.binding {
                    InputBinding::Current(source) => &source.output.node == node,
                    InputBinding::Retained(saved) => &saved.node == node,
                    InputBinding::Unlinked => false,
                });
            if lost {
                instance.snapshot.input = Some(Input {
                    binding: InputBinding::Unlinked,
                    value: None,
                });
                instance.snapshot.input_problem =
                    Some("Input work was removed; bind another result".into());
                instance.snapshot.input_revision =
                    instance.snapshot.input_revision.saturating_add(1);
                instance.snapshot.observing = false;
                instance.input_observation_requested = false;
                self.charged -= instance.charge;
                instance.charge = 0;
            }
            let mut removed = 0;
            for members in instance.snapshot.members.values_mut() {
                let before = members.len();
                members.retain(|id| id != node);
                removed += before - members.len();
            }
            if removed > 0 || lost {
                instance.snapshot.revision = instance.snapshot.revision.saturating_add(1);
                self.edges -= removed;
            }
        }
        self.expire_mounts();
    }
    pub(crate) fn withdraw_dataset_inputs(
        &mut self,
        access: &crate::storage::datasets::DatasetAccess,
    ) {
        for instance in self.instances.values_mut() {
            if instance
                .snapshot
                .input
                .as_ref()
                .and_then(|i| i.value.as_ref())
                .is_some_and(|v| access.blocks(v))
            {
                if let Some(input) = &mut instance.snapshot.input {
                    input.value = None;
                }
                instance.snapshot.input_problem =
                    Some("Input access was withdrawn; cached data was cleared".into());
                instance.snapshot.observing = false;
                instance.input_observation_requested = false;
                instance.snapshot.input_revision =
                    instance.snapshot.input_revision.saturating_add(1);
                instance.snapshot.revision = instance.snapshot.revision.saturating_add(1);
                self.charged -= instance.charge;
                instance.charge = 0;
            }
        }
        self.expire_mounts();
    }
}

impl crate::workspace::Workspace {
    /// Public presentation of a live reference. Resolution and source checks share the owner turn;
    /// callers revalidate after encoding so deletion/privacy changes cannot release an old frame.
    pub fn view_frame(&self, node: &NodeId) -> Result<Frame, String> {
        let root = self.view_frame_base(node)?;
        let mut pending = root
            .instances
            .iter()
            .filter_map(|i| i.query.as_ref().map(|q| q.source.clone()))
            .collect::<Vec<_>>();
        let mut seen = BTreeSet::new();
        while let Some(source) = pending.pop() {
            self.views.read(&source).map_err(|e| e.to_string())?;
            if seen.insert(source.id.clone()) {
                let frame = self.view_frame_base(&source.id)?;
                pending.extend(
                    frame
                        .instances
                        .into_iter()
                        .filter_map(|i| i.query.map(|q| q.source)),
                );
            }
        }
        Ok(root)
    }
    fn view_frame_base(&self, node: &NodeId) -> Result<Frame, String> {
        use crate::runtime::OutputState;
        if self.runtime().is_closed() {
            return Err("Workspace is closed".into());
        }
        let OutputState::Available(value) = self.runtime().output(&OutputRef::data(node.clone()))
        else {
            return Err("View reference is unavailable".into());
        };
        let handle = self.views.resolve(&value).map_err(|e| e.to_string())?;
        let mut frame = self.views.frame(&handle).map_err(|e| e.to_string())?;
        for instance in &mut frame.instances {
            if instance
                .query
                .as_ref()
                .is_some_and(|query| query.adapter.is_some())
            {
                instance.input_delivery = InputDelivery::Window;
            }
            if self.runtime().graph().node(&instance.id).is_none() {
                return Err("A view instance was removed".into());
            }
            if let Some(input) = &instance.input {
                if let InputBinding::Retained(reference) = &input.binding {
                    if self.runtime().graph().node(reference.node()).is_none() {
                        return Err("Retained input work was removed".into());
                    }
                }
                if input.value.as_ref().is_some_and(|v| !public_input(v)) {
                    return Err(
                        "Confidential or unclassified input is not available to a public view"
                            .into(),
                    );
                }
                if let Some(source) = input.source() {
                    instance.input_delivery = if self.has_stream_source(&source.output.node) {
                        InputDelivery::Window
                    } else {
                        InputDelivery::Finite
                    };
                    let definition = self
                        .runtime()
                        .graph()
                        .node(&source.output.node)
                        .ok_or("View source was removed")?
                        .definition();
                    if !source.matches(&source.output, &definition) {
                        return Err(
                            "View source was replaced; explicitly bind the new source".into()
                        );
                    }
                    if (source.output.port == crate::graph::OutputPort::Data
                        && self
                            .runtime()
                            .value_of(&source.output.node)
                            .is_some_and(|v| !public_input(v)))
                        || matches!(self.runtime().output(&source.output), OutputState::Available(v)
                        if !public_input(&v))
                    {
                        return Err("View source is now confidential or unclassified".into());
                    }
                }
            }
        }
        Ok(frame)
    }
}
