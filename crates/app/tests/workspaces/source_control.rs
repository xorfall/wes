//! Finite source-command waits use execution state, never the display cache.
use super::*;
use wes_engine::session::{SequentialState, SessionError, SessionHandle};

async fn wait_results(
    session: &SessionHandle,
    cell: &str,
    updates: &mut tokio::sync::broadcast::Receiver<()>,
) -> Result<(), SessionError> {
    loop {
        let observation = session.observe().await?;
        match SessionHandle::sequential_step_state(&observation, cell) {
            Some(SequentialState::Completed) => return Ok(()),
            None | Some(SequentialState::Running) => (),
            Some(state) => {
                let errors = &observation.state.execution.errors;
                let message = observation
                    .cells
                    .iter()
                    .find(|entry| entry.input.cell() == cell)
                    .and_then(|entry| entry.reply.as_ref())
                    .and_then(|reply| reply.as_ref().ok())
                    .and_then(|reply| {
                        reply
                            .nodes
                            .iter()
                            .chain(&reply.refreshed)
                            .find_map(|id| errors.get(id))
                    })
                    .map(|error| error.message().to_owned())
                    .unwrap_or_else(|| format!("Source result {cell} ended {}", state.as_str()));
                return Err(SessionError::Management(message));
            }
        }
        // Subscribe before submission/observation. A completion between the
        // snapshot and this wait remains queued; lag just requires a new read.
        match updates.recv().await {
            Ok(()) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => (),
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                return Err(SessionError::Stopped);
            }
        }
    }
}

pub(super) async fn source_control(
    app: &ApplicationHandle,
    id: &str,
    client: &str,
    text: &str,
) -> wes_engine::session::SubmissionReply {
    tokio::time::timeout(Duration::from_secs(10), async {
        let session = app.current().unwrap().session;
        let mut updates = session.subscribe_updates()?;
        let reply = app
            .submit(
                SourceInput::new(id.into(), text.into())
                    .unwrap()
                    .with_client(client.into())
                    .unwrap(),
            )
            .await?;
        if let Some(diagnostic) = reply
            .diagnostics
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.severity == wes_language::Severity::Error)
        {
            return Err(SessionError::Management(diagnostic.message.clone()));
        }
        if !reply.nodes.is_empty() || !reply.refreshed.is_empty() {
            wait_results(&session, id, &mut updates).await?;
        }
        Ok(reply)
    })
    .await
    .expect("management must not wait for its own session shutdown")
}

#[tokio::test(start_paused = true)]
async fn held_plan_refresh_keeps_display_data_but_waits_for_current_success_or_failure() {
    use wes_engine::session::management::WorkspaceDeletePlan;
    use wes_engine::session::{RecordingMode, WorkspaceActions, WorkspaceRequest};
    for fail in [false, true] {
        let (actions, mut manager) = WorkspaceActions::channel();
        let workspace =
            Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
        let (session, task) = wes_engine::session::spawn_with_actions(
            workspace,
            RecordingMode::Ephemeral,
            Arc::new(NoFiles),
            NonZeroUsize::new(1).unwrap(),
            None,
            actions,
        )
        .unwrap();
        let mut updates = session.subscribe_updates().unwrap();
        let input = |cell: &str, text: &str| {
            SourceInput::new(cell.into(), text.into())
                .unwrap()
                .with_client("qa".into())
                .unwrap()
        };
        let reply = session
            .submit(input("initial", ":workspace plan delete > plan"))
            .await
            .unwrap();
        let node = reply.nodes[0].clone();
        let WorkspaceRequest::Management(initial) = manager.recv().await.unwrap() else {
            panic!("expected plan request");
        };
        initial
            .reply
            .send(Ok(Some(WorkspaceDeletePlan {
                token: "original authority".into(),
                details: Data::Record(Default::default()),
            })))
            .unwrap();
        wait_results(&session, "initial", &mut updates)
            .await
            .unwrap();
        session
            .submit(input("refresh", ":refresh $plan"))
            .await
            .unwrap();
        let WorkspaceRequest::Management(refresh) = manager.recv().await.unwrap() else {
            panic!("expected refresh request");
        };
        // Hold the actual plan response. This makes the old helper's false
        // completion deterministic instead of relying on a slower CI machine.
        let held = session.observe().await.unwrap();
        assert_eq!(
            held.state.execution.values[&node].management_authority(),
            Some("original authority")
        );
        assert_eq!(
            held.state.execution.graph.node(&node).unwrap().state(),
            wes_engine::graph::NodeState::Running
        );
        let mut waiting = Box::pin(wait_results(&session, "refresh", &mut updates));
        assert!(
            tokio::time::timeout(Duration::from_secs(1), &mut waiting)
                .await
                .is_err()
        );
        if fail {
            refresh
                .reply
                .send(Err("synthetic replanning failure".into()))
                .unwrap();
        } else {
            refresh
                .reply
                .send(Ok(Some(WorkspaceDeletePlan {
                    token: "fresh authority".into(),
                    details: Data::Record(Default::default()),
                })))
                .unwrap();
        }
        let result = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap();
        if fail {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("synthetic replanning failure")
            );
        } else {
            result.unwrap();
            let ready = session.observe().await.unwrap();
            assert_eq!(
                ready.state.execution.values[&node].management_authority(),
                Some("fresh authority")
            );
        }
        session.shutdown().await.unwrap();
        task.join().await.unwrap();
    }
}
