//! Optional application instrumentation; no subscriber, files, configuration or user payloads.
/// A span handle is safe to retain across awaits: it does not enter thread-local context.
pub struct Operation(tracing::Span);
impl Operation {
    pub fn start(operation: &'static str) -> Self {
        Self(
            tracing::info_span!(target: "wes.telemetry", "operation", operation, outcome = tracing::field::Empty),
        )
    }
    pub fn child(&self, operation: &'static str) -> Self {
        Self(
            tracing::info_span!(target: "wes.telemetry", parent: &self.0, "operation", operation, outcome = tracing::field::Empty),
        )
    }
    pub fn span(&self) -> tracing::Span {
        self.0.clone()
    }
    pub fn finish(self, outcome: &'static str) {
        self.0.record("outcome", outcome);
    }
}
