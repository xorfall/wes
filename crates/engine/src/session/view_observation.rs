//! Samples from the shared display-demand owner, not from a renderer-owned stream queue.
use super::*;
impl Actor {
    pub(super) fn sample_view(
        &mut self,
        node: &NodeId,
        identity: &str,
        mount: &str,
    ) -> Result<(), SessionError> {
        let sources = self
            .workspace
            .view_samples(node, identity, mount)
            .map_err(SessionError::AccessDenied)?;
        for (handle, source) in sources {
            let sample = self.displays.read_output(&self.workspace, &source.output);
            let result = match sample {
                Ok(sample) => match sample.value {
                    Some(value) => {
                        let selected = match crate::plan::project_value(&value, &source.fields) {
                            Ok(value) => value,
                            Err(error) => {
                                let _ = self.workspace.views.sample(
                                    &handle,
                                    None,
                                    Some(format!("Observation stopped: {}", error.message())),
                                );
                                continue;
                            }
                        };
                        self.workspace.views.sample_source(
                            &handle,
                            selected,
                            sample.producer_run.clone(),
                        )
                    }
                    None => {
                        self.workspace.views.waiting(&handle);
                        Ok(())
                    }
                },
                Err(error) => {
                    let _ = self.workspace.views.sample(
                        &handle,
                        None,
                        Some(format!("Observation stopped: {error}")),
                    );
                    continue;
                }
            };
            if let Err(error) = result {
                let message = match error {
                    crate::views::Error::Capacity => {
                        "Observation stopped: input exceeds its display budget"
                    }
                    _ => {
                        "Observation stopped: source is unavailable or no longer satisfies the view contract"
                    }
                };
                let _ = self
                    .workspace
                    .views
                    .sample(&handle, None, Some(message.into()));
            }
        }
        Ok(())
    }
}
