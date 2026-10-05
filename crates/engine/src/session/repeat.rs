//! Live repeat intent. Validation, identity admission and refresh share the session actor turn.
use super::*;
use crate::{graph::OutputRef, runtime::OutputState};

pub(super) struct Definition {
    nodes: Vec<(NodeId, Arc<()>)>,
    context: Option<wes_core::environments::EnvironmentContext>,
}

impl Definition {
    pub(super) fn capture(
        workspace: &Workspace,
        nodes: &[NodeId],
        context: Option<wes_core::environments::EnvironmentContext>,
    ) -> Option<Self> {
        if nodes.is_empty() {
            return None;
        }
        let nodes = nodes
            .iter()
            .map(|id| {
                workspace
                    .runtime()
                    .graph()
                    .node(id)
                    .map(|n| (id.clone(), n.definition()))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self { nodes, context })
    }
}

impl Actor {
    pub(super) fn capture_repeat_definition(&mut self, result: &SubmissionResult) {
        if result.nodes.is_empty()
            || result.accepted.len() != 1
            || !result.removed.is_empty()
            || !result.unbound.is_empty()
            || result
                .diagnostics
                .diagnostics
                .iter()
                .any(|d| d.severity == wes_language::Severity::Error)
            || !result.diagnostics.issues.is_empty()
        {
            return;
        }
        let input = self.cells.input(&result.cell).expect("accepted source");
        let context = input
            .environments()
            .cloned()
            .or_else(|| self.environment_clients.get(input.client()).cloned())
            .or_else(|| self.workspace.default_environment_context());
        if let Some(definition) = Definition::capture(&self.workspace, &result.nodes, context) {
            self.cells.set_definition(&result.cell, definition);
        }
    }

    fn repeat_target(
        &self,
        input: &SourceInput,
    ) -> Result<(Vec<NodeId>, Vec<NodeId>), SessionError> {
        let refuse = SessionError::RepeatRefused;
        if self.workspace.runtime().is_closed() {
            return Err(SessionError::Stopped);
        }
        if self.recording_failed {
            return Err(SessionError::Recording);
        }
        if self.checkpoints.busy() {
            return Err(SessionError::CheckpointBusy);
        }
        if self.active.is_some() || !self.environment_work.is_empty() {
            return Err(SessionError::AdmissionBusy);
        }
        let repeat = input.repeat().expect("typed repeat");
        let original = self.cells.input(&repeat.origin).ok_or_else(|| {
            refuse("The original submission is not in this live session. Use New branch.")
        })?;
        let definition = self.cells.definition(&repeat.origin)
            .ok_or_else(|| refuse("Run again requires a successfully declared command or pipeline with unchanged definition evidence."))?;
        if input.text() != original.text() || input.document() != original.document() {
            return Err(refuse(
                "The source changed. Use New branch to submit a different definition.",
            ));
        }
        let context = input
            .environments()
            .cloned()
            .or_else(|| self.environment_clients.get(input.client()).cloned())
            .or_else(|| self.workspace.default_environment_context());
        if context.as_ref().map(|c| &c.selected) != definition.context.as_ref().map(|c| &c.selected)
        {
            return Err(refuse(
                "The environment context changed. Restore the original context or use New branch.",
            ));
        }
        let runtime = self.workspace.runtime();
        let all: Vec<_> = definition.nodes.iter().map(|(id, _)| id.clone()).collect();
        let start = match &repeat.from {
            None => 0,
            Some(from) => all
                .iter()
                .position(|n| n == from)
                .ok_or_else(|| refuse("The selected stage does not belong to this definition."))?,
        };
        let targets = all[start..].to_vec();
        for node in &targets {
            self.source_scope(input)
                .check_effect(node, runtime.graph(), self.workspace.bindings())
                .map_err(SessionError::from_access)?;
        }
        for (id, identity) in &definition.nodes {
            let node = runtime.graph().node(id).ok_or(SessionError::UnknownNode)?;
            if !Arc::ptr_eq(identity, &node.definition()) {
                return Err(refuse(
                    "The node definition changed since this submission. Use New branch.",
                ));
            }
        }
        self.workspace
            .check_refresh_bindings(&targets, wes_language::Span::at(0))
            .map_err(|error| match error {
                crate::workspace::WorkspaceError::Rejected { diagnostics, .. } => {
                    let diagnostic = diagnostics
                        .into_iter()
                        .find(|diagnostic| diagnostic.code == crate::workspace::BINDING_CHANGED)
                        .expect("binding admission diagnostic");
                    SessionError::Environment(wes_core::environments::EnvironmentError {
                        code: crate::workspace::BINDING_CHANGED,
                        message: diagnostic.message,
                    })
                }
                _ => SessionError::UnknownNode,
            })?;
        for id in &targets {
            let node = runtime.graph().node(id).expect("checked definition");
            if runtime.ordered_root(id).is_some() {
                return Err(refuse(
                    "Stream pipelines require an explicit source restart; finite stage rerun cannot replay past events.",
                ));
            }
            let affected = runtime
                .graph()
                .downstream(id)
                .map_err(|_| SessionError::UnknownNode)?;
            if affected.iter().any(|n| runtime.has_lease(n)) {
                return Err(refuse(
                    "This work or its dependents still have outstanding execution. Wait for them to finish before Run again.",
                ));
            }
            if node
                .payload()
                .call()
                .is_some_and(|call| call.streaming() || call.interactive())
            {
                return Err(refuse(
                    "Run again supports finite work; stop and explicitly start streams or interactive work.",
                ));
            }
            if node.dependencies().iter().any(|(node, port)| {
                !targets.contains(node)
                    && !matches!(
                        runtime.output(&OutputRef {
                            node: node.clone(),
                            port: *port
                        }),
                        OutputState::Available(_)
                    )
            }) {
                return Err(refuse(
                    "An input outside the selected stages is unavailable. Resolve upstream work before Run again.",
                ));
            }
            if !repeat.acknowledge_effects
                && (!node.payload().traits().repeatable
                    || affected.iter().any(|n| n != id && !targets.contains(n)))
            {
                return Err(refuse(
                    "Confirm repeat: external actions may repeat and dependent work may be invalidated, cancelled or reactively restarted.",
                ));
            }
        }
        Ok((all, targets))
    }

    pub(super) fn repeat(&mut self, submission: Submission<SourceInput>) {
        let input = submission.source;
        // An acknowledgement retry must return the first outcome, even if eligibility changed.
        if !self.cells.begin(input.clone(), submission.reply) {
            return;
        }
        let (all, targets) = match self.repeat_target(&input) {
            Ok(target) => target,
            Err(error) => {
                self.finish_cell(input.cell(), Err(error));
                return;
            }
        };
        let mut result = SubmissionResult {
            receipts: vec![],
            sandbox: None,
            cell: input.cell().into(),
            nodes: all,
            accepted: vec![],
            removed: vec![],
            unbound: vec![],
            diagnostics: SourceDiagnostics::default(),
            recorded: false,
            restored: false,
            refreshed: vec![],
            repeated_run: None,
        };
        // Reserve reply space before any external execution is authorized.
        if !self
            .cells
            .reserve(input.cell(), identity::charge(&result) + 256)
        {
            self.finish_cell(input.cell(), Err(SessionError::Capacity));
            return;
        }
        match self.workspace.repeat_nodes(&targets, self.io.now()) {
            Ok(effects) => {
                for node in &targets {
                    self.execution_principals.insert(
                        node.clone(),
                        (
                            crate::environments::InvocationAuthority::from_source(&input),
                            input.client().into(),
                        ),
                    );
                }
                result.repeated_run = self.workspace.runtime().run_of(&targets[0]).cloned();
                self.finish_cell(input.cell(), Ok(Arc::new(result)));
                self.effects(effects);
            }
            Err(_) => self.finish_cell(input.cell(), Err(SessionError::Commit)),
        }
    }
}
