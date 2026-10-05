//! Atomic lowering of a pipeline tree into captured nodes and execution-order metadata.
use super::*;
impl DeclarationDraft {
    /// A pipeline is one declaration unit: stage all of it or restore its analysis
    /// snapshot. No provider or import I/O can run here. Prior independent source
    /// statements remain staged under the existing batch contract.
    pub(crate) fn stage_pipeline(
        &mut self,
        stages: &[Statement],
    ) -> Result<Vec<Diagnostic>, WorkspaceError> {
        let saved = (
            self.graph.clone(),
            self.bindings.clone(),
            self.typings.clone(),
            self.recorded_ids.clone(),
            self.changes.len(),
            self.access.clone(),
            self.pipeline_groups.clone(),
        );
        let result = self.stage_pipeline_inner(stages, None, None, &[], &mut None);
        if result.is_err() {
            self.graph = saved.0;
            self.bindings = saved.1;
            self.typings = saved.2;
            self.recorded_ids = saved.3;
            self.changes.truncate(saved.4);
            self.access = saved.5;
            self.pipeline_groups = saved.6;
        }
        result
    }
    fn stage_pipeline_inner(
        &mut self,
        stages: &[Statement],
        mut previous: Option<OutputRef>,
        mut stream_origin: Option<NodeId>,
        branch_path: &[usize],
        last: &mut Option<NodeId>,
    ) -> Result<Vec<Diagnostic>, WorkspaceError> {
        let mut stream_typing = None;
        let mut diagnostics = vec![];
        for (index, written) in stages.iter().enumerate() {
            if let wes_language::Expression::Fork(branches) = &written.expression {
                let input = previous.as_ref().ok_or_else(|| {
                    super::super::rejected(
                        "PIP004",
                        written.span,
                        "fork requires a preceding pipeline input",
                    )
                })?;
                if index + 1 != stages.len()
                    || written.binding.is_some()
                    || written.error_binding.is_some()
                    || !written.annotations.is_empty()
                {
                    return Err(super::super::rejected(
                        "PIP004",
                        written.span,
                        "fork ends its chain and cannot have bindings or annotations; bind each branch result",
                    ));
                }
                for (branch_index, branch) in branches.iter().enumerate() {
                    use crate::graph::OutputPort;
                    use wes_language::BranchSelector;
                    let mut selected = input.clone();
                    selected.port = match &branch.selector {
                        BranchSelector::Value => input.port,
                        BranchSelector::Success => OutputPort::Data,
                        BranchSelector::Failed => OutputPort::Error,
                        BranchSelector::Cancelled => OutputPort::Cancel,
                        BranchSelector::When(_) => input.port,
                    };
                    if stream_origin.as_ref() == Some(&selected.node)
                        && matches!(
                            branch.selector,
                            BranchSelector::Success
                                | BranchSelector::Failed
                                | BranchSelector::Cancelled
                        )
                    {
                        return Err(super::super::rejected(
                            "PIP004",
                            branch.span,
                            "source stream lifecycle selectors require a finite outcome; event branches use a plain block or when",
                        ));
                    }
                    let mut path = branch_path.to_vec();
                    path.push(branch_index);
                    if let BranchSelector::When(name) = &branch.selector {
                        let definition =
                            self.templates.snapshot().get(&name.text).ok_or_else(|| {
                                super::super::rejected(
                                    "PIP004",
                                    name.span,
                                    "unknown predicate definition",
                                )
                            })?;
                        if definition.parameters.len() != 1
                            || !definition.parameters.contains("input")
                            || definition.calculation.as_ref().is_none_or(|d| {
                                d.compiled.effectful()
                                    || d.output.shape()
                                        != wes_core::Shape::Primitive(wes_core::Primitive::Bool)
                            })
                        {
                            return Err(super::super::rejected(
                                "PIP004",
                                name.span,
                                "when requires a pure typed definition with one input parameter and Bool output",
                            ));
                        }
                        let predicate = Statement {
                            annotations: vec![],
                            binding: None,
                            error_binding: None,
                            span: name.span,
                            expression: wes_language::Expression::Call(wes_language::Call {
                                marker: None,
                                path: vec![name.clone()],
                                arguments: vec![],
                                operands: vec![],
                                span: name.span,
                            }),
                        };
                        diagnostics.extend(self.stage_pipeline_inner(
                            &[predicate],
                            Some(selected.clone()),
                            stream_origin.clone(),
                            &path,
                            last,
                        )?);
                        let condition = last.clone().expect("predicate creates a node");
                        let word = |text: &str| wes_language::Name {
                            text: text.into(),
                            span: name.span,
                        };
                        let gate = Statement {
                            annotations: vec![],
                            binding: None,
                            error_binding: None,
                            span: name.span,
                            expression: wes_language::Expression::Call(wes_language::Call {
                                marker: Some(name.span),
                                path: vec![word("stream"), word("filter")],
                                operands: vec![],
                                arguments: vec![wes_language::Argument {
                                    key: word("condition"),
                                    value: wes_language::Value::Reference(word(condition.as_str())),
                                    span: name.span,
                                }],
                                span: name.span,
                            }),
                        };
                        diagnostics.extend(self.stage_pipeline_inner(
                            &[gate],
                            Some(selected),
                            stream_origin.clone(),
                            &path,
                            last,
                        )?);
                        selected = OutputRef::data(last.clone().expect("gate creates a node"));
                    }
                    let body = match &branch.body.expression {
                        wes_language::Expression::Pipeline(stages) => stages.as_slice(),
                        _ => std::slice::from_ref(&branch.body),
                    };
                    diagnostics.extend(self.stage_pipeline_inner(
                        body,
                        Some(selected),
                        stream_origin.clone(),
                        &path,
                        last,
                    )?);
                }
                continue;
            }
            let is_root = previous.is_none();
            let stage = super::super::pipeline::lower(written, previous.as_ref())?;
            let prepared = self.prepare_with_pipe(&stage, previous.as_ref())?;
            let Preparation::Change(mut change) = prepared else {
                return Err(super::super::rejected(
                    "PIP001",
                    stage.span,
                    "meta actions cannot be pipeline stages",
                ));
            };
            let output = match &change.operation {
                Change::Node { node, task, .. }
                    if matches!(
                        task,
                        BoundTask::Call(_)
                            | BoundTask::Calculation(_)
                            | BoundTask::Accumulation(_)
                            | BoundTask::Stream(_)
                            | BoundTask::View(_)
                    ) =>
                {
                    if matches!(task, BoundTask::View(_))
                        && (index + 1 != stages.len() || stream_origin.is_some())
                    {
                        return Err(super::super::rejected(
                            "PIP003",
                            stage.span,
                            "View creation must end a finite chain; reference a committed stream window in a separate presentation pipeline",
                        ));
                    }
                    if task
                        .call()
                        .is_some_and(|call| call.interactive() || (call.streaming() && !is_root))
                    {
                        return Err(super::super::rejected(
                            "PIP003",
                            stage.span,
                            "only the first pipeline stage may stream; later stages must be finite and non-interactive",
                        ));
                    }
                    // Reject two channels from one producer before installing any graph nodes.
                    crate::runtime::OutputSelection::new(task.dependencies()).map_err(|error| {
                        super::super::rejected("PIP002", stage.span, error.to_string())
                    })?;
                    OutputRef::data(node.clone())
                }
                Change::Aliases(_) if is_root => {
                    let wes_language::Expression::Reference(name) = &stage.expression else {
                        unreachable!("first-stage alias")
                    };
                    let output = self.resolve(&name.text).expect("prepared reference");
                    if self
                        .graph
                        .node(&output.node)
                        .and_then(|node| node.payload().call())
                        .is_some_and(|call| call.interactive())
                    {
                        return Err(super::super::rejected(
                            "PIP003",
                            stage.span,
                            "interactive outputs cannot start a finite pipeline; stream references supply their current window",
                        ));
                    }
                    output
                }
                _ => {
                    return Err(super::super::rejected(
                        "PIP001",
                        stage.span,
                        "pipeline stage must create a finite call or calculation",
                    ));
                }
            };
            if let Change::Node {
                stream_origin: origin,
                task,
                node,
                typing,
                pipeline,
                ..
            } = &mut change.operation
            {
                if (matches!(task, BoundTask::Accumulation(_))
                    || matches!(task, BoundTask::Stream(op) if op.requires_events()))
                    && stream_origin.is_none()
                {
                    return Err(super::super::rejected(
                        "ACC001",
                        stage.span,
                        "accumulate requires a new provider stream pipeline; a stream reference supplies a coalescing window",
                    ));
                }
                *origin = stream_origin.clone();
                *pipeline = crate::runtime::PipelineStep {
                    after: last.clone(),
                    branch: branch_path.to_vec(),
                    stateful: matches!(task, BoundTask::Accumulation(_)),
                    limit: match task {
                        BoundTask::Stream(op) => op.limit(),
                        _ => None,
                    },
                };
                // The source lifetime is not a completion barrier for its event consumers.
                *last = Some(node.clone());
                if is_root && task.call().is_some_and(|c| c.streaming()) {
                    stream_origin = Some(node.clone());
                    *last = None;
                    stream_typing = Some((node.clone(), typing.clone()));
                }
            }
            diagnostics.extend_from_slice(change.diagnostics());
            self.stage(change)?;
            if is_root && let Some((node, typing)) = &stream_typing {
                let shape = match &typing.shape {
                    wes_core::Shape::List(item) => (**item).clone(),
                    _ => wes_core::Shape::Unknown,
                };
                self.typings.insert(
                    node.clone(),
                    Arc::new(Typing {
                        shape,
                        provenance: typing.provenance.clone(),
                    }),
                );
            }
            previous = Some(output);
        }
        if let Some((node, typing)) = stream_typing {
            self.typings.insert(node, typing);
        }
        Ok(diagnostics)
    }
}
