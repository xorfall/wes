//! Capture native immutable external inputs; generated data never becomes authored source.
use super::*;
use crate::{
    graph::OutputRef,
    runtime::{ExecutionTraits, OutputState, RestoredState},
    source::SourceInput,
};
use std::collections::BTreeSet;
use wes_language::{Expression, TemplateBody};

impl Workspace {
    pub(crate) fn capture_analysis_inputs(
        &self,
        child: &mut Self,
        program: &str,
        caller: &SourceInput,
    ) -> Result<(), WorkspaceError> {
        let parsed =
            wes_language::parse(&wes_language::SourceText::new("analysis-inputs", program));
        let mut locals = BTreeSet::new();
        let mut references = BTreeSet::new();
        let mut templates = BTreeSet::new();
        let mut pending = parsed.script.statements.iter().collect::<Vec<_>>();
        while let Some(statement) = pending.pop() {
            for binding in statement
                .binding
                .iter()
                .chain(statement.error_binding.iter())
            {
                locals.insert(binding.name.text.clone());
            }
            match &statement.expression {
                Expression::Pipeline(stages)
                | Expression::Sandbox(stages)
                | Expression::Transient(stages) => pending.extend(stages),
                Expression::Fork(branches) => pending.extend(branches.iter().map(|b| &b.body)),
                Expression::Reference(name) => {
                    references.insert(name.text.clone());
                }
                Expression::Call(call) => collect_call(call, &mut references, &mut templates),
                Expression::Calculation(source) => collect_calc(source, &mut references)?,
                Expression::Definition(definition) => {
                    collect_body(&definition.body, &mut references, &mut templates)?
                }
            }
        }
        let mut visited = BTreeSet::new();
        while let Some(name) = templates.pop_first() {
            if !visited.insert(name.clone()) {
                continue;
            }
            if visited.len() > wes_budgets::get("workspace.changes") as usize {
                return Err(rejected(
                    "TRN002",
                    Span::at(0),
                    "Analysis input dependency capacity is full.",
                ));
            }
            if let Some(definition) = self.templates.snapshot().get(&name) {
                collect_body(&definition.syntax.body, &mut references, &mut templates)?;
            }
        }
        let roots = references
            .iter()
            .filter_map(|r| r.split('.').next())
            .collect::<BTreeSet<_>>();
        let mut remaining = wes_budgets::get("values.pending.bytes");
        for name in roots {
            if locals.contains(name) {
                continue;
            }
            let Some(output) = self.resolve(name) else {
                continue;
            };
            let OutputState::Available(value) = self.runtime.output(&output) else {
                return Err(rejected(
                    "TRN002",
                    Span::at(0),
                    "An external analysis input has no current accessible value. No producer was replayed.",
                ));
            };
            if self.dataset_access.blocks(&value)
                || (caller.is_cooperative()
                    && (value.provenance().policy().is_private()
                        || value.provenance().policy().is_unknown()))
            {
                return Err(rejected(
                    "AUT001",
                    Span::at(0),
                    "An external analysis input is not authorized.",
                ));
            }
            if value.shape().contains_meta() || !value.data().is_storable_snapshot() {
                return Err(rejected(
                    "TRN002",
                    Span::at(0),
                    "An analysis input must be a native data snapshot, not live control authority.",
                ));
            }
            let charge = crate::value_size::value_charge(&value, remaining).ok_or_else(|| {
                rejected(
                    "TRN002",
                    Span::at(0),
                    "Analysis input memory capacity is full.",
                )
            })?;
            remaining -= charge;
            let id = child.install_native_input(name, value)?;
            if let Some(BoundTask::SavedInput(saved)) =
                self.runtime.graph().node(&output.node).map(|n| n.payload())
            {
                // Retain the verified immutable artifact identity, not authority inferred
                // from a copied Dataset descriptor or a record's type name.
                child.runtime.replace_held_payload(
                    &id,
                    BoundTask::SavedInput(saved.clone()),
                    ExecutionTraits {
                        pure: false,
                        repeatable: true,
                        bounded: true,
                    },
                )?;
            }
            child.native_origins.insert(
                id,
                crate::history::SavedEdge {
                    node: output.node.to_string(),
                    run: self.runtime.run_of(&output.node).map(ToString::to_string),
                    revision: self.runtime.display_revision(&output.node),
                    port: output.port.selector().into(),
                },
            );
        }
        Ok(())
    }
    pub(crate) fn install_native_input(
        &mut self,
        name: &str,
        value: wes_core::Value,
    ) -> Result<NodeId, WorkspaceError> {
        if self.runtime.graph().len() >= wes_budgets::get("values.outputs") as usize {
            return Err(rejected(
                "TRN002",
                Span::at(0),
                "Native snapshot capacity is full.",
            ));
        }
        let id = NodeId::new(uuid::Uuid::new_v4().to_string()).expect("uuid");
        self.runtime.restore(
            id.clone(),
            BoundTask::Input(value.clone()),
            [],
            ExecutionTraits {
                pure: false,
                repeatable: true,
                bounded: true,
            },
            RestoredState::Ready(value.clone()),
            None,
        )?;
        self.predictions.insert(
            id.clone(),
            Arc::new(wes_core::capability::Typing {
                shape: value.shape().clone(),
                provenance: value.provenance().clone(),
            }),
        );
        self.bindings
            .bind(name, OutputRef::data(id.clone()), self.runtime.graph())
            .map_err(binding_error)?;
        self.native_inputs.insert(id.clone());
        Ok(id)
    }
}
fn collect_call(
    call: &wes_language::Call,
    references: &mut BTreeSet<String>,
    templates: &mut BTreeSet<String>,
) {
    references.extend(
        call.operands
            .iter()
            .chain(call.arguments.iter().map(|a| &a.value))
            .flat_map(wes_language::Value::references)
            .map(|n| n.text.clone()),
    );
    if call.marker.is_none() && call.path.len() == 1 {
        templates.insert(call.path[0].text.clone());
    }
}
fn collect_body(
    body: &TemplateBody,
    references: &mut BTreeSet<String>,
    templates: &mut BTreeSet<String>,
) -> Result<(), WorkspaceError> {
    match body {
        TemplateBody::Call(call) => collect_call(call, references, templates),
        TemplateBody::Calculation(source) => collect_calc(source, references)?,
    }
    Ok(())
}
fn collect_calc(
    source: &wes_language::CalculationSource,
    references: &mut BTreeSet<String>,
) -> Result<(), WorkspaceError> {
    let program = wes_language::calc::parse_context(
        &source.text,
        source.span.start(),
        wes_language::calc::Package::standard(),
    )
    .map_err(|diagnostic| WorkspaceError::Rejected {
        diagnostics: vec![diagnostic],
        issues: vec![],
    })?;
    references.extend(program.expressions.iter().filter_map(|e| match &e.kind {
        wes_language::calc::ExprKind::Workspace(name) => Some(name.clone()),
        _ => None,
    }));
    Ok(())
}
