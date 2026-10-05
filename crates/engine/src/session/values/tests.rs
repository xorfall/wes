use super::*;
use crate::{
    driver::CancellationToken,
    providers::{Call, InvocationFuture, Invoker},
    runtime::{Effect, Outcome},
    storage::{LoadedValue, StoreWorkerLimits, StoreWorkerTask, ValueStore, spawn_store},
    workspace::Preparation,
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, mpsc},
    time::Duration,
};
use wes_core::{
    Data, Provenance, Shape,
    capability::{Capability, ProviderDescription, Safety},
};

type Gate = (oneshot::Sender<()>, mpsc::Receiver<()>);
#[derive(Default)]
struct State {
    values: HashMap<ValueHandle, Value>,
    kept: HashSet<ValueHandle>,
    order: Vec<ValueHandle>,
    keeps: usize,
    gate: Option<Gate>,
    keep_gate: Option<Gate>,
    fail_release: bool,
}
struct Memory(Arc<Mutex<State>>);
impl ValueStore for Memory {
    fn store(&mut self, value: &Value) -> Result<ValueHandle, StoreError> {
        let gate = self.0.lock().unwrap().gate.take();
        if let Some((entered, released)) = gate {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        let handle = ValueHandle::fresh();
        let mut state = self.0.lock().unwrap();
        state.values.insert(handle.clone(), value.clone());
        state.order.push(handle.clone());
        Ok(handle)
    }
    fn read(&self, handle: &ValueHandle) -> Result<Option<LoadedValue>, StoreError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .values
            .get(handle)
            .cloned()
            .map(|value| LoadedValue { value }))
    }
    fn encoded(&self, _: &ValueHandle) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(None)
    }
    fn size(&self, handle: &ValueHandle) -> Result<Option<u64>, StoreError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .values
            .contains_key(handle)
            .then_some(100))
    }
    fn keep(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        let gate = self.0.lock().unwrap().keep_gate.take();
        if let Some((entered, released)) = gate {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        let mut state = self.0.lock().unwrap();
        state.keeps += 1;
        if !state.values.contains_key(handle) {
            return Ok(false);
        }
        state.kept.insert(handle.clone());
        Ok(true)
    }
    fn is_kept(&self, handle: &ValueHandle) -> Result<bool, StoreError> {
        Ok(self.0.lock().unwrap().kept.contains(handle))
    }
    fn release(&mut self, handle: &ValueHandle) -> Result<bool, StoreError> {
        let mut state = self.0.lock().unwrap();
        if state.fail_release {
            return Err(StoreError::backend(
                "release",
                std::io::Error::other("private fixture detail"),
            ));
        }
        state.kept.remove(handle);
        Ok(state.values.remove(handle).is_some())
    }
}
struct Never;
impl Invoker for Never {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        panic!("publication tests never invoke providers")
    }
}
fn value(n: i64) -> Value {
    Value::new(
        Shape::Unknown,
        Data::List(vec![Data::Int(n)]),
        Provenance::default(),
    )
    .unwrap()
}
fn workspace() -> (Workspace, Observation) {
    let mut workspace = Workspace::local(crate::providers::LocalScope::new("fixture").unwrap());
    workspace
        .register_provider(
            ProviderDescription::new(
                "fixture",
                [Capability::new(["value"], Shape::Unknown, Safety::Safe)],
                vec![],
            )
            .unwrap(),
            Arc::new(Never),
        )
        .unwrap();
    let source = wes_language::parse(&wes_language::SourceText::new("fixture", "fixture value"));
    let Preparation::Change(change) = workspace.prepare(&source.script.statements[0]).unwrap()
    else {
        panic!("node")
    };
    workspace.commit(change).unwrap();
    let ticket = workspace
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket),
            _ => None,
        })
        .unwrap();
    let ticket = workspace.enter_ticket(ticket).unwrap().unwrap();
    let observation = workspace
        .complete(&ticket.run, Outcome::Produced(value(0)), Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Observe(observation) if observation.state == NodeState::Ready => {
                Some(observation)
            }
            _ => None,
        })
        .unwrap();
    (workspace, observation)
}
fn fixture() -> (SessionValues, StoreWorkerTask, Arc<Mutex<State>>) {
    let state = Arc::new(Mutex::new(State::default()));
    let (worker, task) = spawn_store(Memory(state.clone()), StoreWorkerLimits::default()).unwrap();
    (
        SessionValues::new(
            SessionStorage {
                worker,
                auto_keep: AutoKeep::default(),
            },
            &RecordingMode::Ephemeral,
        ),
        task,
        state,
    )
}
fn observe(values: &mut SessionValues, observation: &Observation, n: i64) {
    let mut update = observation.clone();
    update.value = Some(value(n));
    assert!(values.observe(&update, true).is_none());
}
async fn drain(values: &mut SessionValues, workspace: &mut Workspace) -> Vec<StorageNotice> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut notices = vec![];
        while !values.is_idle() {
            notices.extend(values.completed(workspace).await.notices);
        }
        notices
    })
    .await
    .unwrap()
}
fn handle(values: &SessionValues, node: &NodeId) -> ValueHandle {
    let ValuePublication::Complete(result) = &values.outputs[node] else {
        panic!("published")
    };
    assert!(result.problem.is_none());
    assert!(result.journal.is_none());
    result.stored.as_ref().unwrap().handle.clone()
}
async fn finish(values: SessionValues, task: StoreWorkerTask) {
    values.storage.shutdown().await.unwrap();
    task.join().await.unwrap();
}
fn gate() -> (Gate, oneshot::Receiver<()>, mpsc::Sender<()>) {
    let (entered, entering) = oneshot::channel();
    let (release, released) = mpsc::channel();
    ((entered, released), entering, release)
}
#[tokio::test]
async fn windows_never_auto_archive_and_reclaim_only_superseded_live_handles() {
    let (mut values, task, state) = fixture();
    let (mut workspace, observation) = workspace();
    for n in 0..4 {
        observe(&mut values, &observation, n);
        assert!(drain(&mut values, &mut workspace).await.is_empty());
        let current = handle(&values, &observation.node);
        let state = state.lock().unwrap();
        assert_eq!(state.values.len(), 1);
        assert_eq!(state.keeps, 0);
        assert_eq!(state.values[&current], value(n));
    }
    assert_eq!(values.bytes, 0);
    finish(values, task).await;
}
#[tokio::test]
async fn late_write_cannot_publish_an_old_window_in_the_same_run_and_pending_windows_coalesce() {
    let (mut values, task, state) = fixture();
    let (mut workspace, observation) = workspace();
    let (gate, entered, release) = gate();
    state.lock().unwrap().gate = Some(gate);
    observe(&mut values, &observation, 0);
    entered.await.unwrap();
    for n in 1..100 {
        observe(&mut values, &observation, n);
    }
    assert_eq!(values.snapshot().pending, 2);
    release.send(()).unwrap();
    assert!(values.completed(&mut workspace).await.notices.is_empty());
    assert!(matches!(
        values.outputs[&observation.node],
        ValuePublication::Pending { .. }
    ));
    assert!(drain(&mut values, &mut workspace).await.is_empty());
    let current = handle(&values, &observation.node);
    {
        let state = state.lock().unwrap();
        assert_eq!(state.order.len(), 2);
        assert_eq!(state.values.len(), 1);
        assert_eq!(state.values[&current], value(99));
    }
    assert_eq!(values.bytes, 0);
    finish(values, task).await;
}
#[tokio::test]
async fn accepted_keep_survives_window_replacement_even_when_its_receipt_arrives_late() {
    let (mut values, task, state) = fixture();
    let (mut workspace, observation) = workspace();
    observe(&mut values, &observation, 0);
    drain(&mut values, &mut workspace).await;
    let kept = handle(&values, &observation.node);
    let (gate, entered, release) = gate();
    state.lock().unwrap().keep_gate = Some(gate);
    let (reply, receive) = oneshot::channel();
    values.request(StorageCommand::Keep, kept.clone(), reply);
    entered.await.unwrap();
    observe(&mut values, &observation, 1);
    observe(&mut values, &observation, 2);
    release.send(()).unwrap();
    assert!(drain(&mut values, &mut workspace).await.is_empty());
    assert!(receive.await.unwrap().unwrap().affected);
    let current = handle(&values, &observation.node);
    assert_ne!(current, kept);
    {
        let state = state.lock().unwrap();
        assert_eq!(state.values.len(), 2);
        assert!(state.kept.contains(&kept));
        assert!(!state.kept.contains(&current));
        assert_eq!(state.values[&current], value(2));
    }
    finish(values, task).await;
}
#[tokio::test]
async fn dropping_a_node_during_publication_cannot_resurrect_it_and_joins_cleanup() {
    let (mut values, task, state) = fixture();
    let (mut workspace, observation) = workspace();
    let (gate, entered, release) = gate();
    state.lock().unwrap().gate = Some(gate);
    observe(&mut values, &observation, 0);
    entered.await.unwrap();
    workspace.drop_node(&observation.node).unwrap();
    values.retain_nodes(&workspace);
    release.send(()).unwrap();
    assert!(drain(&mut values, &mut workspace).await.is_empty());
    assert!(values.outputs.is_empty());
    assert!(values.versions.is_empty());
    assert!(state.lock().unwrap().values.is_empty());
    finish(values, task).await;
}
#[tokio::test]
async fn failed_cleanup_is_reported_without_invalidating_the_new_stored_window() {
    let (mut values, task, state) = fixture();
    let (mut workspace, observation) = workspace();
    observe(&mut values, &observation, 0);
    drain(&mut values, &mut workspace).await;
    let old = handle(&values, &observation.node);
    state.lock().unwrap().fail_release = true;
    observe(&mut values, &observation, 1);
    let notices = drain(&mut values, &mut workspace).await;
    assert_eq!(notices.len(), 1);
    assert_eq!(values.failures, 1);
    assert!(
        matches!(&notices[0].context, NoticeContext::Release { handle, may_have_applied: false } if handle == &old)
    );
    assert!(!notices[0].error.message().contains("private"));
    let current = handle(&values, &observation.node);
    assert_eq!(state.lock().unwrap().values[&current], value(1));
    finish(values, task).await;
}
#[tokio::test]
async fn capacity_refusal_does_not_relabel_the_previous_handle_and_cleanup_respects_queued_keeps() {
    let (mut values, task, state) = fixture();
    let (mut workspace, observation) = workspace();
    observe(&mut values, &observation, 0);
    drain(&mut values, &mut workspace).await;
    let old = handle(&values, &observation.node);
    let (gate, entered, release) = gate();
    state.lock().unwrap().keep_gate = Some(gate);
    let mut replies = vec![];
    for _ in 0..max_pending() {
        let (reply, receive) = oneshot::channel();
        values.request(StorageCommand::Keep, old.clone(), reply);
        replies.push(receive);
    }
    entered.await.unwrap();
    let mut next = observation.clone();
    next.value = Some(value(1));
    assert!(values.observe(&next, true).is_some());
    assert!(values.outputs[&observation.node].handle().is_none());
    release.send(()).unwrap();
    assert!(drain(&mut values, &mut workspace).await.is_empty());
    for reply in replies {
        assert!(reply.await.unwrap().unwrap().affected);
    }
    assert!(state.lock().unwrap().kept.contains(&old));
    assert!(values.outputs[&observation.node].handle().is_none());
    finish(values, task).await;
}
