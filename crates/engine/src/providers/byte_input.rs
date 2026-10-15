//! Observational input capture occurs once at native workspace entry.
use super::*;
use crate::{
    byte_input::{ByteInputSpec, CapturedSource},
    graph::OutputRef,
    workspace::Workspace,
};
impl BoundCall {
    pub(crate) fn require_literal_file_egress(&self) -> Result<(), BindError> {
        if let Some(egress) = self.byte_input_spec().and_then(|spec| spec.file_egress) {
            let destination = matches!(self.invocation.inputs.get(&egress.destination),Some(Input::Literal(value)) if matches!(value.data(),wes_core::Data::Text(_)));
            let approval = match self.invocation.inputs.get(&egress.approval) {
                None => true,
                Some(Input::Literal(value)) => matches!(value.data(), wes_core::Data::Bool(_)),
                _ => false,
            };
            if !destination || !approval {
                return Err(BindError::FileEgressRequiresLiterals);
            }
        }
        Ok(())
    }
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
    pub(crate) fn byte_input_spec(&self) -> Option<ByteInputSpec> {
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
        if !self.provider.invoker.supports_byte_input() {
            return Err(RuntimeCode::ExecutionFailed.error("This target or observation mode does not support owned byte input; no provider was invoked.", None));
        }
        let captured = match self.invocation.inputs.get(&spec.parameter) {
            Some(Input::Literal(value)) => CapturedSource::literal(workspace, value.clone()),
            Some(Input::FromNode(output)) => CapturedSource::capture(workspace, output, vec![]),
            Some(Input::FieldPath{output, fields}) => CapturedSource::capture(workspace, output, fields.clone()),
            _ => return Err(RuntimeCode::InputFailed.error("Byte input requires a literal Bytes value or a static field of an acknowledged result; its producer is not scheduled.", None)),
        }.map_err(|e|e.read_error("Original byte input unavailable; no producer was replayed"))?;
        if let Some(egress) = &spec.file_egress {
            let mut arguments = IndexMap::new();
            for name in [&egress.destination, &egress.approval] {
                match self.invocation.inputs.get(name) {
                    Some(Input::Literal(value)) => { arguments.insert(name.clone(),value.clone()); }
                    None if name == &egress.approval => {}
                    _ => return Err(RuntimeCode::ResultForbidden.error("File export destination and approval must be native literals, independent of source data; no provider was invoked.", None).with_policy(captured.provenance.policy())),
                }
            }
            egress
                .validate(&arguments, captured.provenance.policy())
                .map_err(|message| {
                    RuntimeCode::ResultForbidden
                        .error(message, None)
                        .with_policy(captured.provenance.policy())
                })?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use wes_core::{Data, Primitive, capability::Parameter};
    struct Port;
    impl Invoker for Port {
        fn byte_input(&self, _: &Capability) -> Option<ByteInputSpec> {
            Some(ByteInputSpec {
                parameter: "source".into(),
                maximum: 1024,
                file_egress: Some(crate::byte_input::FileEgress {
                    destination: "file".into(),
                    approval: "approve".into(),
                }),
            })
        }
        fn supports_byte_input(&self) -> bool {
            true
        }
        fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
            panic!("binding or field preparation must not invoke a provider")
        }
    }
    fn literal(data: Data) -> Input {
        Input::Literal(Value::new(Shape::Unknown, data, Default::default()).unwrap())
    }
    fn setup() -> (Providers, Invocation) {
        let mut providers = Providers::local(LocalScope::new("synthetic-egress").unwrap());
        let mut capability = Capability::new(
            ["export"],
            Shape::Primitive(Primitive::Bool),
            Safety::Unsafe,
        );
        capability.parameters = vec![
            Parameter::new("source", Shape::Primitive(Primitive::Bytes), true),
            Parameter::new("file", Shape::Primitive(Primitive::Text), true),
            Parameter::new("approve", Shape::Primitive(Primitive::Bool), false),
        ];
        providers.register(
            ProviderDescription::new("writer", [capability], vec![]).unwrap(),
            Arc::new(Port),
        );
        let provider = providers.catalogue().provider("writer").unwrap().clone();
        let capability = provider.capabilities().next().unwrap().clone();
        let invocation = Invocation {
            provider,
            capability,
            inputs: [
                ("source".into(), literal(Data::Bytes(vec![1].into()))),
                ("file".into(), literal(Data::Text("synthetic.bin".into()))),
                ("approve".into(), literal(Data::Bool(true))),
            ]
            .into(),
            guards: Default::default(),
            cautions: Default::default(),
            interactive: false,
        };
        (providers, invocation)
    }
    fn field(name: &str) -> Input {
        Input::FieldPath {
            output: OutputRef::data(NodeId::new("synthetic-source").unwrap()),
            fields: vec![name.into()],
        }
    }
    #[test]
    fn file_egress_refuses_node_and_field_intent_before_scheduling_or_preparation() {
        let (providers, invocation) = setup();
        assert!(providers.bind_finite(invocation.clone()).is_ok());
        for (parameter, input) in [
            ("file", field("file")),
            ("approve", field("approve")),
            (
                "file",
                Input::FromNode(OutputRef::data(NodeId::new("synthetic-source").unwrap())),
            ),
        ] {
            let mut invalid = invocation.clone();
            invalid.inputs.insert(parameter.into(), input);
            assert_eq!(
                providers.bind_finite(invalid).unwrap_err(),
                BindError::FileEgressRequiresLiterals
            );
        }
    }
    #[test]
    fn field_preparation_preserves_original_byte_owner_and_cannot_mint_export_approval() {
        let (providers, invocation) = setup();
        let mut bound = providers.bind_finite(invocation).unwrap();
        bound
            .invocation
            .inputs
            .insert("source".into(), field("raw"));
        bound.invocation.inputs.insert("file".into(), field("file"));
        bound
            .invocation
            .inputs
            .insert("approve".into(), field("approve"));
        let values = std::collections::BTreeMap::from([
            (
                vec!["raw".into()],
                Value::new(
                    Shape::Unknown,
                    Data::Bytes(vec![1].into()),
                    Default::default(),
                )
                .unwrap(),
            ),
            (
                vec!["file".into()],
                Value::new(
                    Shape::Unknown,
                    Data::Text("synthetic.bin".into()),
                    Default::default(),
                )
                .unwrap(),
            ),
            (
                vec!["approve".into()],
                Value::new(Shape::Unknown, Data::Bool(true), Default::default()).unwrap(),
            ),
        ]);
        bound.prepare_fields(&NodeId::new("synthetic-source").unwrap(), &values);
        for parameter in ["source", "file", "approve"] {
            assert!(matches!(
                bound.invocation.inputs[parameter],
                Input::FieldPath { .. }
            ));
        }
        assert_eq!(
            bound.require_literal_file_egress(),
            Err(BindError::FileEgressRequiresLiterals)
        );
    }
}
