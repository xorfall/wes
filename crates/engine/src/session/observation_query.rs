//! Joined, non-recorded query scopes. The host owns lifetime; renderers receive only samples.
use super::*;

/// Captured language/environment configuration plus a native input. Preparation invokes no provider.
pub struct PreparedObservationQuery {
    workspace: Workspace,
    source: SourceInput,
    live: bool,
}
impl PreparedObservationQuery {
    pub fn capture(
        workspace: &Workspace,
        template: &str,
        input: wes_core::Value,
        caller: &SourceInput,
    ) -> Result<Self, WorkspaceError> {
        let (workspace, source) = workspace.observation_workspace(template, input, caller)?;
        Ok(Self {
            workspace,
            source,
            live: false,
        })
    }
    pub fn capture_live(
        workspace: &Workspace,
        template: &str,
        adapter: &str,
        input: wes_core::Value,
        caller: &SourceInput,
    ) -> Result<Self, WorkspaceError> {
        let (workspace, source) =
            workspace.observation_workspace_mapped(template, Some(adapter), input, caller)?;
        Ok(Self {
            workspace,
            source,
            live: true,
        })
    }
    /// Capacity must be the host application's shared pool, not a pool per view.
    pub async fn start(
        self,
        reader: Arc<dyn TypeSourceReader>,
        capacity: crate::driver::ExecutionCapacity,
    ) -> Result<ObservationQuery, SessionError> {
        let expected_nodes = if self.live { 2 } else { 1 };
        let concurrency = NonZeroUsize::new(capacity.subscribe().borrow().operations.limit)
            .ok_or(SessionError::Capacity)?;
        let recording = RecordingMode::Ephemeral;
        let (handle, task) = spawn_owned(
            self.workspace,
            recording.clone(),
            reader,
            concurrency,
            SessionSeed {
                capacity: Some(capacity),
                actions: None,
                cells: Cells::default(),
                log: SessionLog::new(&recording),
                values: None,
                restoration: None,
            },
        )?;
        // Install the guard before the first await: cancellation of start also joins cleanup.
        let mut query = ObservationQuery {
            owned: Some((handle, task)),
            output: None,
        };
        let submitted = query.handle().submit(self.source).await;
        let result = match submitted {
            Ok(result)
                if !result
                    .diagnostics
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == wes_language::Severity::Error)
                    && result.nodes.len() == expected_nodes =>
            {
                result
            }
            Ok(_) => {
                query.close().await?;
                return Err(SessionError::AccessDenied(
                    "View query could not be prepared; no result is available".into(),
                ));
            }
            Err(error) => {
                query.close().await?;
                return Err(error);
            }
        };
        query.output = result.nodes.last().cloned();
        Ok(query)
    }
}
/// A single query owns its child runtime, not any source node in the containing workspace.
/// Explicit close joins workers; abandoning the handle schedules the same joined cleanup.
pub struct ObservationQuery {
    owned: Option<(SessionHandle, SessionTask)>,
    output: Option<NodeId>,
}
impl ObservationQuery {
    fn handle(&self) -> &SessionHandle {
        &self.owned.as_ref().expect("live query").0
    }
    /// Demand-sampled, byte-bounded input. This does not retain an event history.
    pub async fn sample(&self) -> Result<DisplaySample, SessionError> {
        self.handle()
            .display_value(self.output.clone().ok_or(SessionError::Preparation)?)
            .await
    }
    /// A stream can be idle between updates while still owning its subscription.
    pub async fn live_active(&self) -> Result<bool, SessionError> {
        let snapshot = self.handle().snapshot().await?;
        if !snapshot.execution.errors.is_empty() {
            return Err(SessionError::UnknownValue);
        }
        Ok(snapshot.admission_pending
            || !snapshot.execution.streaming.is_empty()
            || !snapshot.execution.executing.is_empty()
            || !snapshot.execution.idle)
    }
    pub async fn wait_idle(&self) -> Result<(), SessionError> {
        self.handle().wait_idle().await
    }
    pub fn subscribe_updates(&self) -> Result<broadcast::Receiver<()>, SessionError> {
        self.handle().subscribe_updates()
    }
    /// True means some remote effect/outcome could not be confirmed during cleanup.
    pub async fn close(mut self) -> Result<bool, SessionError> {
        close(self.owned.take().expect("live query")).await
    }
}
async fn close((handle, task): (SessionHandle, SessionTask)) -> Result<bool, SessionError> {
    let _ = handle.shutdown().await;
    task.join().await.map_err(|_| SessionError::Stopped)?;
    Ok(handle.remote_outcome_uncertain())
}
impl Drop for ObservationQuery {
    fn drop(&mut self) {
        if let Some(owned) = self.owned.take() {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(close(owned));
            }
            // With no running Tokio runtime, dropping the only sender closes the child mailbox.
        }
    }
}
