//! Output bindings retain identities, not copies of source payloads. Input overlays are demand-read.
use super::*;
use wes_core::contracts::ContractKind;
#[derive(Clone, Debug)]
pub(super) struct Binding {
    pub source: Handle,
    pub port: String,
}
#[derive(Clone, Debug, Default)]
pub struct InputPatches {
    pub values: BTreeMap<NodeId, BTreeMap<String, Value>>,
    pub problems: BTreeMap<NodeId, String>,
    pub cautions: BTreeMap<NodeId, BTreeSet<String>>,
    pub revisions: Vec<String>,
}
impl Store {
    pub fn link(
        &mut self,
        source: &Handle,
        port: &str,
        target: &Handle,
        field: &str,
        revision: u64,
    ) -> Result<u64, Error> {
        let from = self.instance(source)?;
        let to = self.editable(target, revision)?;
        let output = from
            .snapshot
            .definition
            .manifest
            .outputs
            .get(port)
            .filter(|p| p.shared)
            .ok_or(Error::Interaction)?;
        let contract = from
            .snapshot
            .definition
            .contracts
            .resolve(&if output.mode == wes_views::Mode::Event {
                format!("List<{}>", output.r#type)
            } else {
                output.r#type.clone()
            })
            .map_err(|_| Error::Interaction)?;
        let input = to.snapshot.definition.input();
        let ContractKind::Record(fields) = input.kind() else {
            return Err(Error::Input);
        };
        let expected = &fields.get(field).ok_or(Error::Input)?.contract;
        if !contract.is_subtype_of(expected) {
            return Err(Error::Incompatible);
        }
        if to.bindings.contains_key(field) {
            return Err(Error::Duplicate);
        }
        if to.bindings.len() >= wes_budgets::get("view.bindings") as usize
            || self
                .instances
                .values()
                .map(|i| i.bindings.len())
                .sum::<usize>()
                >= wes_budgets::get("view.bindings.total") as usize
        {
            return Err(Error::Capacity);
        }
        if self.binding_reaches(&target.id, &source.id) || self.reaches(&target.id, &source.id) {
            return Err(Error::Cycle);
        }
        let instance = self
            .instances
            .get_mut(&target.id)
            .expect("validated target");
        instance.bindings.insert(
            field.into(),
            Binding {
                source: source.clone(),
                port: port.into(),
            },
        );
        instance.snapshot.revision += 1;
        instance.snapshot.linked_inputs = instance.bindings.keys().cloned().collect();
        Ok(instance.snapshot.revision)
    }
    pub fn unlink(&mut self, target: &Handle, field: &str, revision: u64) -> Result<u64, Error> {
        let instance = self.editable(target, revision)?;
        if !instance.bindings.contains_key(field) {
            return Err(Error::NotMember);
        }
        let instance = self
            .instances
            .get_mut(&target.id)
            .expect("validated target");
        instance.bindings.remove(field);
        instance.snapshot.revision += 1;
        instance.snapshot.linked_inputs = instance.bindings.keys().cloned().collect();
        Ok(instance.snapshot.revision)
    }
    pub(super) fn binding_reaches(&self, from: &NodeId, to: &NodeId) -> bool {
        let mut pending = vec![from.clone()];
        let mut seen = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if &id == to {
                return true;
            }
            if seen.insert(id.clone()) {
                if let Some(instance) = self.instances.get(&id) {
                    pending.extend(instance.snapshot.members.values().flatten().cloned());
                }
                pending.extend(
                    self.instances
                        .values()
                        .filter(|i| {
                            i.bindings.values().any(|b| b.source.id == id)
                                || i.snapshot.query.as_ref().is_some_and(|q| q.source.id == id)
                        })
                        .map(|i| i.handle.id.clone()),
                );
            }
        }
        false
    }
}
impl crate::workspace::Workspace {
    pub fn view_input_patches(
        &self,
        node: &NodeId,
        identity: &str,
    ) -> Result<InputPatches, String> {
        let frame = self.view_frame(node)?;
        if frame
            .instances
            .first()
            .is_none_or(|i| i.identity.as_ref() != identity)
        {
            return Err("View identity changed".into());
        }
        let mut result = InputPatches::default();
        let mut charge = 0;
        for target in frame.instances {
            result
                .revisions
                .push(format!("{}:{}", target.identity, target.revision));
            let instance = self.views.instances.get(&target.id).ok_or("View removed")?;
            let mut fields = BTreeMap::new();
            for (field, binding) in &instance.bindings {
                let read = (|| {
                    self.views
                        .instance(&binding.source)
                        .map_err(|e| e.to_string())?;
                    let (owner, state) =
                        self.view_interaction(&binding.source.id, &binding.source.identity)?;
                    result.revisions.push(format!(
                        "{}:{}:{}",
                        owner.identity, owner.revision, state.revision
                    ));
                    let value = self
                        .views
                        .output(&binding.source, &binding.port)
                        .map_err(|_| "Waiting for a committed source output".to_string())?;
                    charge += crate::value_size::value_charge(&value, 32 * 1024)
                        .ok_or("Input binding exceeds its value budget")?;
                    if charge > 256 * 1024 {
                        return Err("Input bindings exceed their 256 KiB display budget".into());
                    }
                    Ok(value)
                })();
                match read {
                    Ok(value) => {
                        result
                            .cautions
                            .entry(target.id.clone())
                            .or_default()
                            .extend(value.provenance().cautions().iter().cloned());
                        fields.insert(field.clone(), value);
                    }
                    Err(problem) => {
                        result.problems.insert(target.id.clone(), problem);
                    }
                }
            }
            if !instance.bindings.is_empty() {
                result.values.insert(target.id, fields);
            }
        }
        Ok(result)
    }
}
