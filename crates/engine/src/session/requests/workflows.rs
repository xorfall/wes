//! Finite request sequencing. Durable step identities are evidence, never resume authority.
use super::*;
use crate::{
    driver::CancellationToken,
    graph::NodeState,
    session::{SessionError, SessionObservation},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SequentialState {
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}
impl SequentialState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }
}
pub(super) struct Workflow {
    actor: String,
    stop: CancellationToken,
    state: SequentialState,
    gate: Arc<tokio::sync::Mutex<()>>,
}

/// None means this step is still pending, not permission to dispatch another step.
pub fn step_state(observation: &SessionObservation, cell: &str) -> Option<SequentialState> {
    let cell = observation.cells.iter().find(|c| c.input.cell() == cell)?;
    let reply = cell.reply.as_ref()?;
    let Ok(reply) = reply else {
        return Some(SequentialState::Failed);
    };
    if reply
        .diagnostics
        .diagnostics
        .iter()
        .any(|d| d.severity == wes_language::Severity::Error)
    {
        return Some(SequentialState::Failed);
    }
    let graph = &observation.state.execution;
    for id in reply.nodes.iter().chain(&reply.refreshed) {
        if graph.streaming.contains(id) {
            return Some(SequentialState::Interrupted);
        }
        match graph.graph.node(id).map(|n| n.state()) {
            Some(NodeState::Ready) => (),
            Some(NodeState::Pending | NodeState::Running) => return None,
            Some(NodeState::Cancelled) => return Some(SequentialState::Cancelled),
            Some(NodeState::Failed) => return Some(SequentialState::Failed),
            _ => return Some(SequentialState::Interrupted),
        }
    }
    Some(SequentialState::Completed)
}

impl SessionHandle {
    pub fn sequential_state(&self, cell: &str) -> Option<SequentialState> {
        self.requests
            .workflows
            .lock()
            .expect("request workflows")
            .get(cell)
            .map(|w| w.state)
    }
    pub fn sequential_step_state(
        observation: &SessionObservation,
        cell: &str,
    ) -> Option<SequentialState> {
        step_state(observation, cell)
    }
    pub(super) fn start_workflow(
        &self,
        record: &RequestRecord,
        inputs: Vec<SourceInput>,
        terminal_stop: CancellationToken,
        credit: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<(), RecordError> {
        let actor = inputs[0].client().to_owned();
        let stop = CancellationToken::new();
        let gate = Arc::new(tokio::sync::Mutex::new(()));
        {
            let mut workflows = self.requests.workflows.lock().expect("request workflows");
            if workflows.len() >= KNOWN_REQUESTS {
                let expired = workflows
                    .iter()
                    .find(|(_, w)| w.state != SequentialState::Running)
                    .map(|(key, _)| key.clone())
                    .ok_or(RecordError::ReadBusy)?;
                workflows.shift_remove(&expired);
            }
            workflows.insert(
                record.cell.clone(),
                Workflow {
                    actor: actor.clone(),
                    stop: stop.clone(),
                    state: SequentialState::Running,
                    gate: gate.clone(),
                },
            );
        }
        let session = self.clone();
        let root = record.cell.clone();
        tokio::spawn(async move {
            let _credit = credit;
            let mut updates = match session.subscribe_updates() {
                Ok(u) => u,
                Err(_) => {
                    session.finish_workflow(&root, SequentialState::Interrupted);
                    return;
                }
            };
            let mut outcome = SequentialState::Completed;
            for input in inputs {
                let admission = gate.lock().await;
                if stop.is_cancelled() || terminal_stop.is_cancelled() {
                    outcome = SequentialState::Cancelled;
                    break;
                }
                let cell = input.cell().to_owned();
                // Admission owns publication once submitted. Do not abandon or retry it on a lost wait.
                if session.submit(input).await.is_err() {
                    outcome = SequentialState::Interrupted;
                    break;
                }
                drop(admission);
                loop {
                    if stop.is_cancelled() || terminal_stop.is_cancelled() {
                        let _ = session.cancel_actor_work(actor.clone(), cell.clone()).await;
                        outcome = SequentialState::Cancelled;
                        break;
                    }
                    let observation = match session.observe().await {
                        Ok(o) => o,
                        Err(_) => {
                            outcome = SequentialState::Interrupted;
                            break;
                        }
                    };
                    if let Some(state) = step_state(&observation, &cell) {
                        outcome = state;
                        if state == SequentialState::Interrupted {
                            // An open stream is outside the finite workflow contract.
                            let _ = session.cancel_actor_work(actor.clone(), cell.clone()).await;
                        }
                        break;
                    }
                    tokio::select! {
                        _=stop.cancelled()=>(),
                        _=terminal_stop.cancelled()=>(),
                        update=updates.recv()=>if matches!(update,Err(tokio::sync::broadcast::error::RecvError::Closed)) { outcome=SequentialState::Interrupted;break; },
                    }
                }
                if outcome != SequentialState::Completed {
                    break;
                }
            }
            session.finish_workflow(&root, outcome);
        });
        Ok(())
    }
    fn finish_workflow(&self, cell: &str, state: SequentialState) {
        if let Some(workflow) = self
            .requests
            .workflows
            .lock()
            .expect("request workflows")
            .get_mut(cell)
        {
            workflow.state = state;
        }
        if let Some(updates) = self.updates.upgrade() {
            let _ = updates.send(());
        }
    }
    /// Check all admitted effects as one actor operation before stopping future steps.
    pub async fn cancel_request_work(
        &self,
        actor: String,
        record: &RequestRecord,
    ) -> Result<(), SessionError> {
        if record.steps.is_empty() {
            return self.cancel_actor_work(actor, record.cell.clone()).await;
        }
        let gate = self
            .requests
            .workflows
            .lock()
            .expect("request workflows")
            .get(&record.cell)
            .map(|w| w.gate.clone());
        let _admission = match &gate {
            Some(gate) => Some(gate.try_lock().map_err(|_| SessionError::AdmissionBusy)?),
            None => None,
        };
        let observation = self.observe().await?;
        let nodes = observation
            .cells
            .iter()
            .filter(|c| record.steps.contains(&c.input.cell().to_owned()))
            .filter_map(|c| c.reply.as_ref().and_then(|r| r.as_ref().ok()))
            .flat_map(|r| r.nodes.iter().chain(&r.refreshed))
            .cloned()
            .collect::<Vec<_>>();
        let own = self
            .requests
            .workflows
            .lock()
            .expect("request workflows")
            .get(&record.cell)
            .is_some_and(|w| w.actor == actor);
        if nodes.is_empty() && !own {
            return Err(SessionError::Authority);
        }
        // This checks the complete cancellation scope and refuses atomically on an authority failure.
        self.cancel_actor(actor, nodes).await?;
        if let Some(workflow) = self
            .requests
            .workflows
            .lock()
            .expect("request workflows")
            .get(&record.cell)
        {
            workflow.stop.cancel();
        }
        Ok(())
    }
}
