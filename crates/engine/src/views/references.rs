//! Reference identity and captured origin. Bytes become Retained only with an acknowledged receipt.
use super::{Input, Source};
use crate::{
    graph::{NodeId, OutputPort},
    runtime::RunId,
    storage::ValueHandle,
};
use wes_core::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InputDelivery {
    #[default]
    Finite,
    Window,
}
impl InputDelivery {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Finite => "finite",
            Self::Window => "window",
        }
    }
}

#[derive(Clone, Debug)]
pub enum InputBinding {
    Unlinked,
    Current(Source),
    Retained(RetainedReference),
}
impl InputBinding {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Unlinked => "unlinked",
            Self::Current(_) => "current",
            Self::Retained(_) => "retained",
        }
    }
    pub fn current(&self) -> bool {
        matches!(self, Self::Current(_))
    }
}

/// Audit metadata travels separately from retained ownership and definition authority.
#[derive(Clone, Debug)]
pub struct SourceAudit {
    pub node: NodeId,
    pub port: OutputPort,
    pub fields: Vec<String>,
    pub run: Option<RunId>,
    pub digest: String,
}
impl From<&Source> for SourceAudit {
    fn from(source: &Source) -> Self {
        Self {
            node: source.output.node.clone(),
            port: source.output.port,
            fields: source.fields.clone(),
            run: source.run.clone(),
            digest: source.digest.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct RetainedReference {
    pub(crate) node: NodeId,
    pub(crate) run: RunId,
    pub(crate) handle: ValueHandle,
    pub(crate) digest: String,
    pub(crate) origin: Option<SourceAudit>,
}
impl RetainedReference {
    pub fn node(&self) -> &NodeId {
        &self.node
    }
    pub fn run(&self) -> &RunId {
        &self.run
    }
    pub fn handle(&self) -> &ValueHandle {
        &self.handle
    }
    pub fn origin(&self) -> Option<&SourceAudit> {
        self.origin.as_ref()
    }
}
impl Input {
    pub(crate) fn retained(reference: RetainedReference, value: Value) -> Self {
        Self {
            binding: InputBinding::Retained(reference),
            value: Some(value),
        }
    }
}

/// A bounded continuation owned by the accepted Pin run; never checkpointed or replayed.
#[derive(Clone, Debug)]
pub(crate) struct PinIntent {
    pub handle: super::Handle,
    pub revision: u64,
    pub value: Value,
    pub origin: Option<SourceAudit>,
    pub principal: Option<(crate::environments::InvocationAuthority, String)>,
}
impl crate::workspace::Workspace {
    pub(crate) fn capture_pin(
        &self,
        handle: &super::Handle,
        revision: u64,
        scope: &crate::access::WriteScope,
    ) -> Result<PinIntent, String> {
        self.check_view_access(handle, scope)
            .map_err(|e| e.to_string())?;
        let frame = self.view_frame(handle.id())?;
        let snapshot = frame.instances.first().ok_or("View is unavailable")?;
        if snapshot.revision != revision {
            return Err(super::Error::Revision.to_string());
        }
        if !snapshot.linked_inputs.is_empty() {
            return Err(
                "Pin does not support linked input fields yet; capture the effective input first"
                    .into(),
            );
        }
        let input = snapshot.input.as_ref().ok_or("View has no input to pin")?;
        let value = input.value().ok_or("View input is unavailable")?;
        if !value.provenance().policy().allows_retention() || !value.data().is_storable_snapshot() {
            return Err("Pin requires a materialized input whose policy permits retention".into());
        }
        if snapshot.input_problem.is_some() {
            return Err("Resolve the view input problem before Pin".into());
        }
        self.views
            .check_input(
                &snapshot.definition,
                Some(input),
                self.views
                    .instance(handle)
                    .map_err(|e| e.to_string())?
                    .charge,
            )
            .map_err(|e| e.to_string())?;
        let origin = match &input.binding {
            InputBinding::Current(source) => Some(SourceAudit::from(source)),
            InputBinding::Retained(reference) => reference.origin.clone(),
            InputBinding::Unlinked => None,
        };
        Ok(PinIntent {
            handle: handle.clone(),
            revision,
            value: value.clone(),
            origin,
            principal: None,
        })
    }
    /// Only SessionValues can construct this continuation after protected storage and journal ack.
    pub(crate) fn bind_kept_pin(
        &mut self,
        pin: &crate::session::values::KeptPin,
        scope: &crate::access::WriteScope,
    ) -> Result<(), String> {
        self.check_view_access(&pin.intent.handle, scope)
            .map_err(|e| e.to_string())?;
        // Recheck source authority/privacy and configuration. Input sampling alone does not cancel
        // a deliberate capture of an older displayed window.
        self.view_frame(pin.intent.handle.id())?;
        if self.runtime().graph().node(&pin.node).is_none() {
            return Err("Pin work was removed before binding".into());
        }
        let reference = RetainedReference {
            node: pin.node.clone(),
            run: pin.run.clone(),
            handle: pin.handle.clone(),
            digest: super::persistence::input_digest(&pin.intent.value),
            origin: pin.intent.origin.clone(),
        };
        self.views
            .bind(
                &pin.intent.handle,
                pin.intent.revision,
                Some(Input::retained(reference, pin.intent.value.clone())),
            )
            .map_err(|e| e.to_string())?;
        self.views.update_query_intents();
        Ok(())
    }
}
