//! Observational input capture occurs once at native workspace entry.
use super::*;
use crate::{
    byte_input::{ByteInputSpec, CapturedSource},
    graph::OutputRef,
    workspace::Workspace,
};
impl BoundCall {
    pub(crate) fn stored_byte_source_for(&self, node: &NodeId) -> bool {
        self.byte_read_sources()
            .iter()
            .any(|source| &source.node == node)
            && !self.ordinary_inputs().any(|(_, input)| {
                input
                    .dependencies()
                    .iter()
                    .any(|source| &source.node == node)
            })
            && !self
                .context
                .pipe_input
                .as_ref()
                .is_some_and(|source| &source.node == node)
    }
    pub(super) fn byte_input_spec(&self) -> Option<ByteInputSpec> {
        self.provider
            .invoker
            .byte_input(&self.invocation.capability)
    }
    pub(super) fn ordinary_inputs(&self) -> impl Iterator<Item = (&String, &Input)> {
        let parameter = self.byte_input_spec().map(|s| s.parameter);
        self.invocation
            .inputs
            .iter()
            .filter(move |(name, _)| parameter.as_ref() != Some(*name))
    }
    pub(crate) fn byte_read_sources(&self) -> Vec<OutputRef> {
        self.byte_input_spec()
            .and_then(|spec| self.invocation.inputs.get(&spec.parameter))
            .and_then(|input| match input {
                Input::FromNode(output) | Input::FieldPath { output, .. } => Some(output.clone()),
                _ => None,
            })
            .into_iter()
            .collect()
    }
    pub(crate) fn capture_byte_input(&mut self, workspace: &Workspace) -> Result<(), ErrorValue> {
        let Some(spec) = self.byte_input_spec() else {
            return Ok(());
        };
        if !self.provider.invoker.supports_byte_input() || self.invocation.trace_profile.is_some() {
            return Err(RuntimeCode::ExecutionFailed.error("This target or observation mode does not support owned byte input; no provider was invoked.", None));
        }
        let captured = match self.invocation.inputs.get(&spec.parameter) {
            Some(Input::Literal(value)) => CapturedSource::literal(workspace, value.clone()),
            Some(Input::FromNode(output)) => CapturedSource::capture(workspace, output, vec![]),
            Some(Input::FieldPath{output, fields}) => CapturedSource::capture(workspace, output, fields.clone()),
            _ => return Err(RuntimeCode::InputFailed.error("Byte input requires a literal Bytes value or a static field of an acknowledged result; its producer is not scheduled.", None)),
        }.map_err(|e|e.read_error("Original byte input unavailable; no producer was replayed"))?;
        let shape = match &captured.source {
            crate::byte_input::Source::Resident(value) => value.shape(),
            crate::byte_input::Source::Stored(snapshot) => &snapshot.shape,
        };
        if crate::plan::field_shape(shape, &captured.fields)
            != Some(Shape::Primitive(wes_core::Primitive::Bytes))
        {
            return Err(RuntimeCode::InputFailed
                .error("Original byte input must select Bytes.", None)
                .with_policy(captured.provenance.policy()));
        }
        let placeholder = Value::new(
            Shape::Primitive(wes_core::Primitive::Bytes),
            wes_core::Data::Bytes(vec![].into()),
            captured.provenance.clone(),
        )
        .expect("empty native byte placeholder");
        self.invocation
            .inputs
            .insert(spec.parameter, Input::Literal(placeholder));
        self.context.byte_input = Some(captured);
        Ok(())
    }
}
