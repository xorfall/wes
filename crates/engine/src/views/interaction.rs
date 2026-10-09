//! Committed interaction is workspace state. Hover/draft fields never enter this store.
use super::*;
use wes_core::{Provenance, Shape, contracts::ContractKind};

#[derive(Clone, Debug, Default)]
pub struct InteractionState {
    pub revision: u64,
    pub fields: BTreeMap<String, Data>,
    pub outputs: BTreeMap<String, Data>,
}
#[derive(Clone, Debug)]
pub struct InteractionEdit {
    pub events: Vec<EventEmission>,
    pub owner: NodeId,
    pub identity: String,
    pub definition_revision: u64,
    pub revision: u64,
    pub fields: BTreeMap<String, Data>,
    pub outputs: BTreeMap<String, Data>,
}
impl Store {
    pub fn output(&self, handle: &Handle, port: &str) -> Result<Value, Error> {
        let (owner, state) = self.interaction(handle)?;
        let output = owner
            .definition
            .manifest
            .outputs
            .get(port)
            .filter(|p| p.shared)
            .ok_or(Error::Interaction)?;
        let contract = owner
            .definition
            .contracts
            .resolve(&output.r#type)
            .map_err(|_| Error::Interaction)?;
        let provenance = Provenance::default()
            .with_fact("view.instance", owner.identity.as_ref())
            .with_fact("view.output", port)
            .with_fact("view.revision", state.revision.to_string());
        if output.mode == wes_views::Mode::Event {
            return self.instances[&owner.id]
                .events
                .read(port, contract.shape(), provenance);
        }
        let data = state.outputs.get(port).ok_or(Error::Interaction)?.clone();
        Value::new(contract.shape(), data, provenance).map_err(|_| Error::Interaction)
    }

    /// A coordinating slot is the sole state owner, including when a member is opened alone.
    pub fn interaction_owner(&self, handle: &Handle) -> Result<Handle, Error> {
        self.instance(handle)?;
        let mut id = handle.id.clone();
        for _ in 0..=self.limits.instances {
            let parent = self.instances.values().find(|i| {
                i.snapshot.members.iter().any(|(slot, members)| {
                    i.snapshot.definition.manifest.slots[slot].coordinates && members.contains(&id)
                })
            });
            match parent {
                Some(parent) => id = parent.handle.id.clone(),
                None => return Ok(self.instances[&id].handle.clone()),
            }
        }
        Err(Error::Cycle)
    }
    pub fn interaction(&self, handle: &Handle) -> Result<(Snapshot, InteractionState), Error> {
        let owner = self.interaction_owner(handle)?;
        // A coordinator's public state can observe every member's input. Check
        // the complete owned frame, including separately opened children.
        if self.frame(&owner)?.instances.iter().any(|instance| {
            instance
                .input
                .as_ref()
                .and_then(Input::value)
                .is_some_and(|v| !public_input(v))
        }) {
            return Err(Error::Interaction);
        }
        let instance = self.instance(&owner)?;
        if instance.snapshot.definition.manifest.interaction.is_none() {
            return Err(Error::Interaction);
        }
        Ok((instance.snapshot.clone(), instance.interaction.clone()))
    }
    pub fn commit_interaction(
        &mut self,
        handle: &Handle,
        edit: InteractionEdit,
    ) -> Result<InteractionState, Error> {
        self.interaction(handle)?;
        let owner = self.interaction_owner(handle)?;
        let instance = self.instance(&owner)?;
        if owner.id != edit.owner || owner.identity.as_ref() != edit.identity {
            return Err(Error::Reference);
        }
        if instance.snapshot.revision != edit.definition_revision
            || instance.interaction.revision != edit.revision
        {
            return Err(Error::Revision);
        }
        let package = &instance.snapshot.definition;
        let interaction = package
            .manifest
            .interaction
            .as_ref()
            .ok_or(Error::Interaction)?;
        let contract = package
            .contracts
            .resolve(&interaction.state)
            .map_err(|_| Error::Interaction)?;
        let ContractKind::Record(fields) = contract.kind() else {
            return Err(Error::Interaction);
        };
        if edit.fields.len() != interaction.shared_fields.len()
            || edit.outputs.len()
                != package
                    .manifest
                    .outputs
                    .values()
                    .filter(|p| p.shared && p.mode == wes_views::Mode::State)
                    .count()
        {
            return Err(Error::Interaction);
        }
        let mut charge = 0;
        for (name, data) in &edit.fields {
            if !interaction.shared_fields.contains(name) {
                return Err(Error::Interaction);
            }
            let contract = &fields.get(name).ok_or(Error::Interaction)?.contract;
            validate(data, contract, &mut charge)?;
        }
        for (name, data) in &edit.outputs {
            let port = package
                .manifest
                .outputs
                .get(name)
                .filter(|p| p.shared && p.mode == wes_views::Mode::State)
                .ok_or(Error::Interaction)?;
            let contract = package
                .contracts
                .resolve(&port.r#type)
                .map_err(|_| Error::Interaction)?;
            validate(data, &contract, &mut charge)?;
        }
        let events = instance.events.append(package, &edit.events)?;
        let revision = edit.revision.checked_add(1).ok_or(Error::Capacity)?;
        let state = InteractionState {
            revision,
            fields: edit.fields,
            outputs: edit.outputs,
        };
        let instance = self.instances.get_mut(&owner.id).expect("checked owner");
        instance.interaction = state.clone();
        instance.events = events;
        self.update_query_intents();
        Ok(state)
    }
}
fn validate(
    data: &Data,
    contract: &wes_core::contracts::Contract,
    charge: &mut u64,
) -> Result<(), Error> {
    let value = Value::new(Shape::Unknown, data.clone(), Provenance::default())
        .map_err(|_| Error::Interaction)?;
    *charge += crate::value_size::value_charge(&value, 16384).ok_or(Error::Capacity)?;
    if *charge > 16384 {
        return Err(Error::Capacity);
    }
    if !data.is_inline() || !contract.issues(data).is_empty() {
        return Err(Error::Interaction);
    }
    Ok(())
}
impl crate::workspace::Workspace {
    /// User presentation API; this never grants an agent mutation capability.
    pub fn view_interaction(
        &self,
        node: &NodeId,
        identity: &str,
    ) -> Result<(Snapshot, InteractionState), String> {
        let frame = self.view_frame(node)?;
        let root = frame.instances.first().ok_or("View unavailable")?;
        if root.identity.as_ref() != identity {
            return Err("View identity changed".into());
        }
        let handle = &self
            .views
            .instances
            .get(&root.id)
            .ok_or("View unavailable")?
            .handle;
        let (owner, state) = self.views.interaction(handle).map_err(|e| e.to_string())?;
        // Check the coordinator's own sources/privacy too, even for a separately opened member.
        let owned = self.view_frame(&owner.id)?;
        if owned.instances.iter().any(|entry| {
            entry.input.is_none()
                && self
                    .views
                    .instances
                    .get(&entry.id)
                    .is_some_and(|i| i.snapshot.input.is_some())
        }) {
            return Err("Shared interaction input is unavailable".into());
        }
        Ok((owner, state))
    }
    pub fn commit_view_interaction(
        &mut self,
        node: &NodeId,
        identity: &str,
        edit: InteractionEdit,
    ) -> Result<InteractionState, String> {
        let (owner, _) = self.view_interaction(node, identity)?;
        if owner.id != edit.owner {
            return Err("View coordinator changed".into());
        }
        let handle = self
            .views
            .instances
            .get(&owner.id)
            .ok_or("View unavailable")?
            .handle
            .clone();
        self.views
            .commit_interaction(&handle, edit)
            .map_err(|e| e.to_string())
    }
}
