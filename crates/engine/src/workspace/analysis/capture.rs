//! Lower public capture syntax into the existing source and finite companion owners.
use super::*;
use crate::providers::profiles::ProfileSelector;
impl Analysis<'_> {
    pub(super) fn capture_start(
        &self,
        task: plan::MetaTask,
        statement: &Statement,
    ) -> Result<(BoundTask, Arc<Typing>), WorkspaceError> {
        let invalid = |message: &str| rejected("TRC002", statement.span, message);
        if self.pipe_input.is_some()
            || !statement.annotations.is_empty()
            || !task.subjects.is_empty()
        {
            return Err(invalid(
                "Capture start requires a standalone named profile and literal narrowing options.",
            ));
        }
        let mut options = IndexMap::new();
        let mut name = None;
        for (key, input) in task.inputs {
            let plan::Input::Literal(value) = input else {
                return Err(invalid(
                    "Capture options must be literals; no input producer was started.",
                ));
            };
            let value = match (key.as_str(), value.data()) {
                ("capture", wes_core::Data::Text(text)) => {
                    name = Some(text.to_string());
                    continue;
                }
                ("duration", wes_core::Data::Text(text)) => {
                    let n = wes_language::capture::milliseconds(text, 120000)
                        .filter(|n| *n > 0)
                        .ok_or_else(|| {
                            invalid("Capture duration must be positive bounded ms, s or m Text.")
                        })?;
                    wes_core::Value::new(
                        Shape::Primitive(wes_core::Primitive::Int),
                        wes_core::Data::Int(n as i64),
                        value.provenance().clone(),
                    )
                    .expect("duration")
                }
                ("filter", wes_core::Data::Text(_))
                | ("packets" | "bytes", wes_core::Data::Int(_)) => value,
                _ => return Err(invalid("Unknown or malformed capture option.")),
            };
            options.insert(key, value);
        }
        let profile =
            self.providers
                .resolve_source_profile(
                    &ProfileSelector::Named(name.ok_or_else(|| {
                        invalid("Capture start requires an explicit profile name.")
                    })?),
                    None,
                    &options,
                    None,
                )
                .map_err(|e| invalid(&e.to_string()))?;
        if !profile.records_bytes() {
            return Err(invalid(
                "Trace capture requires an original raw recording declaration.",
            ));
        }
        let source = profile.with_recording_schema(self.contracts).into_source();
        let typing = Arc::new(source.invocation().predicted_typing(|_| None));
        Ok((BoundTask::Call(source), typing))
    }
    pub(super) fn capture_companion(
        &self,
        call: crate::providers::BoundCall,
        statement: &Statement,
    ) -> Result<BoundTask, WorkspaceError> {
        let Some(annotation) = statement
            .annotations
            .iter()
            .find(|a| a.name.text == "trace")
        else {
            return Ok(BoundTask::Call(call));
        };
        let invalid = |message: &str| rejected("TRC001", statement.span, message);
        let request =
            wes_language::capture::CaptureAnnotation::parse(annotation).map_err(invalid)?;
        let selector = request.name.map_or(
            ProfileSelector::Default("tcp".into()),
            ProfileSelector::Named,
        );
        let profile = self
            .providers
            .resolve_source_profile(&selector, Some(&call), &IndexMap::new(), request.tail)
            .map_err(|e| invalid(&e.to_string()))?;
        if !profile.records_bytes() {
            return Err(invalid(
                "Trace capture requires an original raw recording declaration.",
            ));
        }
        profile
            .with_recording_schema(self.contracts)
            .into_companion(call)
            .map(|task| BoundTask::Companion(Box::new(task)))
            .map_err(|e| invalid(e.message()))
    }
}
