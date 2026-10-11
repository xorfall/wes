//! Explicit, immutable invocation requests. A pending request owns neither a worker
//! nor permission to publish; the session revalidates it after physical exit.
use super::*;
use crate::{providers::BoundCall, runtime::OutputState, selection::Selection};
use wes_core::{Data, Shape, Value};

pub(super) struct Pending {
    pub(super) input: SourceInput,
    call: BoundCall,
    definition: Arc<()>,
    _credit: OwnedSemaphorePermit,
}
impl Actor {
    fn selection_value(&self, selection: &Selection) -> Result<Value, SessionError> {
        let refuse = || {
            SessionError::RepeatRefused(
                "Selection source or version is no longer available. Activate a current item.",
            )
        };
        let OutputState::Available(value) = self.workspace.runtime().output(&selection.output())
        else {
            return Err(refuse());
        };
        match selection {
            Selection::Rendered {
                source,
                run,
                view,
                artifact,
                value: chosen,
            } => {
                if self.workspace.runtime().is_streaming(source)
                    || self.workspace.runtime().value_run(source) != Some(run)
                {
                    return Err(refuse());
                }
                let package = crate::selection::rendered_package(
                    value.shape(),
                    self.workspace.view_catalogue(),
                )
                .ok_or_else(refuse)?;
                if package.manifest.id != *view || package.artifact.as_deref() != Some(artifact) {
                    return Err(refuse());
                }
                let contract = crate::selection::selection_contract(package).ok_or_else(refuse)?;
                if !contract.issues(chosen).is_empty() {
                    return Err(refuse());
                }
                let (shape, data) = match (contract.shape(), chosen) {
                    (Shape::Option(inner), Data::Option(Some(data))) => {
                        (*inner, data.as_ref().clone())
                    }
                    (Shape::Option(_), _) => return Err(refuse()),
                    (shape, data) => (shape, data.clone()),
                };
                Value::new(
                    shape,
                    data,
                    value
                        .provenance()
                        .clone()
                        .with_fact("selection.source", source.as_str())
                        .with_fact("selection.run", run.as_str())
                        .with_fact("selection.view", view),
                )
                .map_err(|_| refuse())
            }
            Selection::Row { source, run, row } => {
                if self.workspace.runtime().is_streaming(source)
                    || self.workspace.runtime().value_run(source) != Some(run)
                {
                    return Err(refuse());
                }
                let (Shape::List(item), Data::List(rows)) = (value.shape(), value.data()) else {
                    return Err(refuse());
                };
                Value::new(
                    item.as_ref().clone(),
                    rows.get(*row).ok_or_else(refuse)?.clone(),
                    value
                        .provenance()
                        .clone()
                        .with_fact("selection.source", source.as_str())
                        .with_fact("selection.run", run.as_str())
                        .with_fact("selection.row", row.to_string()),
                )
                .map_err(|_| refuse())
            }
            Selection::View {
                identity,
                revision,
                port,
                ..
            } => {
                let handle = self.workspace.views.resolve(&value).map_err(|_| refuse())?;
                let (owner, state) = self
                    .workspace
                    .views
                    .interaction(&handle)
                    .map_err(|_| refuse())?;
                let expected_port = if owner.definition.manifest.outputs.contains_key("selected") {
                    "selected"
                } else {
                    "selection"
                };
                if owner.identity.as_ref() != identity
                    || state.revision != *revision
                    || port != expected_port
                {
                    return Err(refuse());
                }
                let output = self
                    .workspace
                    .views
                    .output(&handle, port)
                    .map_err(|_| refuse())?;
                match (output.shape(), output.data()) {
                    (Shape::Option(inner), Data::Option(Some(data))) => Value::new(
                        inner.as_ref().clone(),
                        data.as_ref().clone(),
                        output.provenance().clone(),
                    )
                    .map_err(|_| refuse()),
                    (Shape::Option(_), _) => Err(refuse()),
                    _ => Ok(output),
                }
            }
        }
    }
    fn capture_activation(
        &self,
        input: &SourceInput,
        call: &BoundCall,
    ) -> Result<BoundCall, SessionError> {
        let selector = input.selection().ok_or(SessionError::Preparation)?;
        if call.selection_sources().is_empty()
            || call
                .selection_sources()
                .iter()
                .any(|s| **s != selector.output())
            || call.streaming()
            || call.interactive()
        {
            return Err(SessionError::RepeatRefused(
                "Activation requires one finite provider call bound to this selection source.",
            ));
        }
        let value = self.selection_value(selector)?;
        let inputs = call
            .dependencies()
            .map(|output| match self.workspace.runtime().output(&output) {
                OutputState::Available(value) => Ok((output.node, value)),
                _ => Err(SessionError::RepeatRefused(
                    "An invocation input is unavailable.",
                )),
            })
            .collect::<Result<IndexMap<_, _>, _>>()?;
        let mut captured = call
            .capture_selection(&selector.output(), &value, &inputs)
            .map_err(|_| {
                SessionError::RepeatRefused(
                    "Selection arguments do not satisfy the provider contract.",
                )
            })?;
        captured.set_invocation_request(input.cell().into());
        captured.set_authority(crate::environments::InvocationAuthority::from_source(input));
        captured
            .validate_captured()
            .map_err(|_| SessionError::Authority)?;
        Ok(captured)
    }
    pub(super) fn prepare_first_activation(
        &self,
        prepared: &PreparedSource,
    ) -> Result<Option<BoundCall>, SessionError> {
        match (prepared.input().selection(), prepared.selection_call()) {
            (Some(_), Some(call)) => self.capture_activation(prepared.input(), call).map(Some),
            (None, _) => Ok(None),
            _ => Err(SessionError::RepeatRefused(
                "Activation requires exactly one selection-bound provider declaration.",
            )),
        }
    }
    fn activation_target(&self, input: &SourceInput) -> Result<NodeId, SessionError> {
        if self.workspace.runtime().is_closed() {
            return Err(SessionError::Stopped);
        }
        if self.recording_failed {
            return Err(SessionError::Recording);
        }
        if self.checkpoints.busy() {
            return Err(SessionError::CheckpointBusy);
        }
        let repeat = input.repeat().ok_or(SessionError::Preparation)?;
        let original = self
            .cells
            .input(&repeat.origin)
            .ok_or(SessionError::UnknownNode)?;
        let definition = self
            .cells
            .definition(&repeat.origin)
            .ok_or(SessionError::UnknownNode)?;
        if input.text() != original.text()
            || input.document() != original.document()
            || repeat.from.is_some()
            || definition.nodes.len() != 1
        {
            return Err(SessionError::RepeatRefused(
                "The authored definition changed; create a new waiting cell.",
            ));
        }
        let context = input
            .environments()
            .cloned()
            .or_else(|| self.environment_clients.get(input.client()).cloned())
            .or_else(|| self.workspace.default_environment_context());
        if context.as_ref().map(|c| &c.selected) != definition.context.as_ref().map(|c| &c.selected)
        {
            return Err(SessionError::RepeatRefused(
                "The invocation environment changed.",
            ));
        }
        let (id, identity) = &definition.nodes[0];
        let node = self
            .workspace
            .runtime()
            .graph()
            .node(id)
            .ok_or(SessionError::UnknownNode)?;
        if !Arc::ptr_eq(identity, &node.definition()) {
            return Err(SessionError::RepeatRefused(
                "The target definition changed.",
            ));
        }
        self.source_scope(input)
            .check_effect(
                id,
                self.workspace.runtime().graph(),
                self.workspace.bindings(),
            )
            .map_err(SessionError::from_access)?;
        self.workspace
            .check_refresh_bindings(std::slice::from_ref(id), Span::at(0))
            .map_err(|_| {
                SessionError::RepeatRefused("The target's environment binding changed.")
            })?;
        Ok(id.clone())
    }
    pub(super) fn activate_selection(&mut self, submission: Submission<SourceInput>) {
        let input = submission.source;
        if !self.cells.begin(input.clone(), submission.reply) {
            return;
        }
        let result = (|| {
            let id = self.activation_target(&input)?;
            let node = self
                .workspace
                .runtime()
                .graph()
                .node(&id)
                .ok_or(SessionError::UnknownNode)?;
            let call = node.payload().call().ok_or(SessionError::Preparation)?;
            let captured = self.capture_activation(&input, call)?;
            let definition = node.definition();
            if !self.cells.reserve(input.cell(), 8192) {
                return Err(SessionError::Capacity);
            }
            // All checks precede revocation. Invalid selections cannot disturb current work.
            let effects = self
                .workspace
                .supersede_invocation(&id)
                .map_err(|_| SessionError::Commit)?;
            if let Some(previous) = self.selection_pending.insert(
                id,
                Pending {
                    input: input.clone(),
                    call: captured,
                    definition,
                    _credit: submission.credit,
                },
            ) {
                self.finish_cell(
                    previous.input.cell(),
                    Err(SessionError::RepeatRefused(
                        "Superseded by a newer activation before execution.",
                    )),
                );
            }
            self.effects(effects);
            Ok(())
        })();
        if let Err(error) = result {
            self.finish_cell(input.cell(), Err(error));
        }
        self.drain_activations();
    }
    pub(super) fn cancel_activations(&mut self, nodes: &[NodeId]) {
        for node in nodes {
            if let Some(pending) = self.selection_pending.remove(node) {
                self.finish_cell(pending.input.cell(), Err(SessionError::Cancelled));
            }
        }
    }
    pub(super) fn drain_activations(&mut self) {
        if self.active.is_some() || !self.environment_work.is_empty() || self.checkpoints.busy() {
            return;
        }
        let ready: Vec<_> = self
            .selection_pending
            .keys()
            .filter(|node| !self.workspace.runtime().has_lease(node))
            .cloned()
            .collect();
        for id in ready {
            let pending = self
                .selection_pending
                .remove(&id)
                .expect("pending activation");
            let outcome = (|| {
                self.activation_target(&pending.input)?;
                let node = self
                    .workspace
                    .runtime()
                    .graph()
                    .node(&id)
                    .ok_or(SessionError::UnknownNode)?;
                if !Arc::ptr_eq(&pending.definition, &node.definition()) {
                    return Err(SessionError::RepeatRefused(
                        "Pending activation stopped: the definition changed.",
                    ));
                }
                pending
                    .call
                    .validate_captured()
                    .map_err(|_| SessionError::Authority)?;
                let effects = self
                    .workspace
                    .invoke_captured(&id, pending.call, self.io.now())
                    .map_err(|_| SessionError::Commit)?;
                self.execution_principals.insert(
                    id.clone(),
                    (
                        crate::environments::InvocationAuthority::from_source(&pending.input),
                        pending.input.client().into(),
                    ),
                );
                let result = SubmissionResult {
                    receipts: vec![],
                    sandbox: None,
                    cell: pending.input.cell().into(),
                    nodes: vec![id.clone()],
                    accepted: vec![],
                    removed: vec![],
                    unbound: vec![],
                    diagnostics: SourceDiagnostics::default(),
                    recorded: false,
                    restored: false,
                    refreshed: vec![id.clone()],
                    repeated_run: self.workspace.runtime().run_of(&id).cloned(),
                };
                self.effects(effects);
                Ok(Arc::new(result))
            })();
            if outcome.is_err()
                && self
                    .workspace
                    .runtime()
                    .graph()
                    .node(&id)
                    .is_some_and(|node| Arc::ptr_eq(&pending.definition, &node.definition()))
            {
                let effects = self.workspace.cancel(&id, self.io.now());
                self.effects(effects);
            }
            self.finish_cell(pending.input.cell(), outcome);
        }
    }
}

impl SessionHandle {
    /// Analyse authoring intent without creating a node, recording source or executing work.
    pub async fn selection_template(&self, text: String) -> Result<Option<NodeId>, SessionError> {
        self.selection_template_in(text, None).await
    }
    pub async fn selection_template_in(
        &self,
        text: String,
        context: Option<wes_core::environments::EnvironmentContext>,
    ) -> Result<Option<NodeId>, SessionError> {
        if text.len() > source::max_source_bytes() {
            return Err(SessionError::Capacity);
        }
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::SelectionTemplate(text, context, reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
}
