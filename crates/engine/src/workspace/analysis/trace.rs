//! Local tracing facades select captured finite provider operations; no packet backend lives here.
use super::*;
use crate::plan::Input;
use wes_core::{Data, Value};

impl Analysis<'_> {
    pub(super) fn trace_finite(
        &self,
        mut task: plan::MetaTask,
        statement: &Statement,
    ) -> Result<(BoundTask, Arc<Typing>), WorkspaceError> {
        let invalid = |message: &str| rejected("TRC003", statement.span, message);
        if self.pipe_input.is_some() || !statement.annotations.is_empty() {
            return Err(invalid(
                "Local trace operations require explicit standalone arguments without annotations or pipe input.",
            ));
        }
        let kind = match task.spec.command {
            MetaCommand::TraceImport => "capture.import",
            MetaCommand::TraceAnalyze => "capture.analyze",
            MetaCommand::TraceExport => "bytes.export",
            _ => unreachable!(),
        };
        let Some(Input::Literal(using)) = task.inputs.shift_remove("using") else {
            return Err(invalid(
                "Select an explicit literal finite profile using using:\"name\"; no executable is inferred.",
            ));
        };
        let Data::Text(name) = using.data() else {
            return Err(invalid("Profile name must be literal Text."));
        };
        let mut inputs = IndexMap::new();
        if let Some(options) = task.inputs.shift_remove("options") {
            if !options.dependencies().is_empty() || !options.selection_sources().is_empty() {
                return Err(invalid(
                    "Profile options must be a bounded literal record; no option producer is run.",
                ));
            }
            let value = options
                .resolve(&IndexMap::new())
                .map_err(|_| invalid("Profile options must be a bounded literal record."))?
                .into_owned();
            let Data::Record(fields) = value.data() else {
                return Err(invalid("Profile options must be a literal record."));
            };
            if fields.len() > 28 || crate::value_size::value_charge(&value, 128 * 1024).is_none() {
                return Err(invalid(
                    "Profile options exceed their finite metadata budget.",
                ));
            }
            for (key, data) in fields {
                if matches!(
                    key.as_str(),
                    "source" | "file" | "using" | "options" | "allow_confidential"
                ) {
                    return Err(invalid(
                        "Profile options cannot replace the original source, destination, profile or egress approval.",
                    ));
                }
                let shape = value.shape().field(key).cloned().unwrap_or(Shape::Unknown);
                let field = Value::new(shape, data.clone(), value.provenance().clone())
                    .map_err(|_| invalid("Malformed typed profile option."))?;
                inputs.insert(key.clone(), Input::Literal(field));
            }
        }
        for (key, input) in task.inputs {
            if !matches!(input, Input::Literal(_)) {
                return Err(invalid(
                    "Trace file and disclosure approval must be native literals; no argument producer is run.",
                ));
            }
            inputs.insert(key, input);
        }
        if task.spec.command == MetaCommand::TraceImport {
            if !task.subjects.is_empty() {
                return Err(invalid(
                    "Trace import takes file: and using:, without a result operand.",
                ));
            }
        } else {
            let [source] = task.subjects.as_slice() else {
                return Err(invalid("Select one original static Bytes result or field."));
            };
            if !matches!(source,Input::FromNode(output)|Input::FieldPath{output,..} if output.port==OutputPort::Data)
            {
                return Err(invalid(
                    "Select an original static data Bytes reference; copied descriptors and literals grant no capture ownership.",
                ));
            }
            inputs.insert("source".into(), source.clone());
        }
        let call = self
            .providers
            .resolve_finite_profile(name, kind, &inputs)
            .map_err(|error| invalid(&error.to_string()))?;
        if !call.is_local_target() {
            return Err(invalid(
                "This trace operation is local only; remote capture and transfer are not implemented.",
            ));
        }
        if task.spec.command != MetaCommand::TraceImport {
            let spec=call.byte_input_spec().ok_or_else(||invalid("This finite profile must declare the native owned byte-input port; no provider was invoked."))?;
            if call.finite_profile_parameter("source").as_ref() != Some(&spec.parameter) {
                return Err(invalid(
                    "The profile's canonical source must map to its owned byte-input parameter.",
                ));
            }
            if task.spec.command == MetaCommand::TraceExport {
                let egress = spec.file_egress.ok_or_else(|| {
                    invalid(
                        "Export profiles must declare explicit file_egress permission metadata.",
                    )
                })?;
                if call.finite_profile_parameter("file").as_ref() != Some(&egress.destination)
                    || call.finite_profile_parameter("allow_confidential").as_ref()
                        != Some(&egress.approval)
                {
                    return Err(invalid(
                        "Export profile destination and approval must map to the declared file_egress parameters.",
                    ));
                }
            }
        }
        let typing = Arc::new(
            call.invocation()
                .predicted_typing(|node| self.data_typing(node)),
        );
        Ok((BoundTask::Call(call), typing))
    }
}
