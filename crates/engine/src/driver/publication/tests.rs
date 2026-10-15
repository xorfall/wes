use super::*;
use crate::{
    driver::ExecutionHold,
    history::Persistence,
    runtime::{Effect, ExecutionTraits},
    storage::{Retention, StoredOutput, StoredSnapshot, ValueHandle},
};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use wes_core::{Data, Primitive, Shape, Value};

fn value() -> Value {
    Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(7),
        Default::default(),
    )
    .unwrap()
}
fn entered() -> (Runtime<()>, Run) {
    let mut runtime = Runtime::new();
    runtime
        .add(
            (),
            [],
            ExecutionTraits {
                pure: false,
                repeatable: false,
                bounded: true,
            },
        )
        .unwrap();
    let run = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            Effect::Spawn(ticket) => Some(ticket.run),
            _ => None,
        })
        .unwrap();
    assert!(runtime.enter_joined(&run));
    (runtime, run)
}
struct Held(Arc<AtomicUsize>);
impl Drop for Held {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn publication_shares_original_holds_and_credits_until_acknowledgement_and_join() {
    let (mut runtime, run) = entered();
    let pool = Arc::new(Semaphore::new(65536));
    let (send, mut receive) = mpsc::channel(1);
    let reporter = Reporter::new(run.clone(), send, pool.clone(), Arc::default());
    let dropped = Arc::new(AtomicUsize::new(0));
    let mut report: ExecutionReport = Outcome::Produced(value()).into();
    report
        .holds
        .push(ExecutionHold::retain(Held(dropped.clone())));
    let original = report.clone();
    let publishing = tokio::spawn(async move { reporter.publish(&report).await });
    let queued = receive.recv().await.unwrap();
    assert!(pool.available_permits() < 65536);
    assert!(!publishing.is_finished());
    queued.apply(&mut runtime, Duration::ZERO);
    assert!(publishing.await.unwrap());
    assert_eq!(pool.available_permits(), 65536);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    runtime.complete(&run, Outcome::Skipped, Duration::ZERO);
    assert_eq!(runtime.value_of(run.node()), Some(&value()));
    drop(original);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn abandoned_acknowledgement_and_exhausted_credits_never_publish_or_replay() {
    let (mut runtime, run) = entered();
    let (send, mut receive) = mpsc::channel(1);
    let report: ExecutionReport = Outcome::Produced(value()).into();
    let tiny = Reporter::new(
        run.clone(),
        send.clone(),
        Arc::new(Semaphore::new(1)),
        Arc::default(),
    );
    assert!(!tiny.publish(&report).await);
    assert!(receive.try_recv().is_err());
    let pool = Arc::new(Semaphore::new(65536));
    let reporter = Reporter::new(run.clone(), send, pool.clone(), Arc::default());
    let publishing = tokio::spawn(async move { reporter.publish(&report).await });
    let queued = receive.recv().await.unwrap();
    publishing.abort();
    let _ = publishing.await;
    assert!(pool.available_permits() < 65536);
    assert!(queued.apply(&mut runtime, Duration::ZERO).is_empty());
    assert!(runtime.value_of(run.node()).is_none());
    assert_eq!(pool.available_permits(), 65536);
    runtime.complete(&run, Outcome::Skipped, Duration::ZERO);
}

#[tokio::test]
async fn expired_generation_refuses_queued_result_even_after_a_new_grant() {
    let (mut runtime, run) = entered();
    let (send, mut receive) = mpsc::channel(1);
    let reporter = Reporter::new(
        run.clone(),
        send,
        Arc::new(Semaphore::new(65536)),
        Arc::default(),
    );
    let generation = Arc::new(AtomicU64::new(1));
    let mut report: ExecutionReport = Outcome::Produced(value()).into();
    report.publication_guard = Some(Guard::new({
        let generation = generation.clone();
        move || generation.load(Ordering::SeqCst) == 1
    }));
    let publishing = tokio::spawn(async move { reporter.publish(&report).await });
    let queued = receive.recv().await.unwrap();
    generation.store(2, Ordering::SeqCst);
    queued.apply(&mut runtime, Duration::ZERO);
    assert!(
        publishing.await.unwrap(),
        "the refusal itself is an acknowledged terminal outcome"
    );
    assert!(runtime.value_of(run.node()).is_none());
    assert_eq!(runtime.error_of(run.node()).unwrap().code(), "RUN009");
    runtime.complete(&run, Outcome::Produced(value()), Duration::ZERO);
    assert!(runtime.value_of(run.node()).is_none());
}

#[tokio::test]
async fn stored_descriptor_is_published_without_materialization_or_weakening_its_policy() {
    let (mut runtime, run) = entered();
    let handle = ValueHandle::fresh();
    let value = value().with_provenance(wes_core::Provenance::default().with_policy(
        &wes_core::flow::FlowPolicy::default().confidential(wes_core::flow::Residence::Retainable),
    ));
    let snapshot =
        StoredSnapshot::capture(handle.clone(), format!("sha256:{}", "a".repeat(64)), &value)
            .unwrap();
    let output = StoredOutput {
        handle,
        snapshot: Some(snapshot.clone()),
        bytes: 512,
        kept: false,
        retention: Retention::Automatic,
        retained_persistence: Persistence::Volatile,
        requires_ownership_record: false,
    };
    let report: ExecutionReport = Outcome::Stored(output).into();
    let (send, mut receive) = mpsc::channel(1);
    let reporter = Reporter::new(
        run.clone(),
        send,
        Arc::new(Semaphore::new(65536)),
        Arc::default(),
    );
    let publishing = tokio::spawn(async move { reporter.publish(&report).await });
    receive
        .recv()
        .await
        .unwrap()
        .apply(&mut runtime, Duration::ZERO);
    assert!(publishing.await.unwrap());
    assert!(runtime.value_of(run.node()).is_none());
    assert_eq!(
        runtime.stored_output(run.node()).unwrap().snapshot,
        snapshot
    );
    runtime.complete(&run, Outcome::Skipped, Duration::ZERO);
    assert_eq!(
        runtime.stored_output(run.node()).unwrap().snapshot,
        snapshot
    );
}

#[test]
fn terminal_charge_bounds_error_notice_and_descriptor_work_before_cloning() {
    let limit = wes_budgets::get("execution.publication.value.bytes");
    let report: ExecutionReport =
        Outcome::Failed(RuntimeCode::ExecutionFailed.error("x".repeat(limit as usize), None))
            .into();
    assert!(charge(&report).is_none());
    let mut report: ExecutionReport = Outcome::Produced(value()).into();
    report.notices = vec![RuntimeCode::ExecutionFailed.error("small", None); 129];
    assert!(charge(&report).is_none());
    let handle = ValueHandle::fresh();
    let snapshot = StoredSnapshot::capture(
        handle.clone(),
        format!("sha256:{}", "a".repeat(64)),
        &value(),
    )
    .unwrap();
    let output = StoredOutput {
        handle,
        snapshot: Some(snapshot.with_selection(vec!["x".repeat(limit as usize)])),
        bytes: 512,
        kept: false,
        retention: Retention::Automatic,
        retained_persistence: Persistence::Volatile,
        requires_ownership_record: false,
    };
    assert!(charge(&Outcome::Stored(output).into()).is_none());
    let handle = ValueHandle::fresh();
    let snapshot =
        StoredSnapshot::capture(handle.clone(), "x".repeat(limit as usize), &value()).unwrap();
    let output = StoredOutput {
        handle,
        snapshot: Some(snapshot),
        bytes: 512,
        kept: false,
        retention: Retention::Automatic,
        retained_persistence: Persistence::Volatile,
        requires_ownership_record: false,
    };
    assert!(charge(&Outcome::Stored(output).into()).is_none());
}

#[tokio::test]
async fn cancelled_and_duplicate_result_publications_do_not_release_or_replace_the_owner() {
    for cancelled in [false, true] {
        let (mut runtime, run) = entered();
        let report: ExecutionReport = Outcome::Produced(value()).into();
        let (send, mut receive) = mpsc::channel(1);
        let pool = Arc::new(Semaphore::new(65536));
        let reporter = Reporter::new(run.clone(), send, pool.clone(), Arc::default());
        let publishing = tokio::spawn(async move { reporter.publish(&report).await });
        let update = receive.recv().await.unwrap();
        if cancelled {
            runtime.cancel(run.node(), Duration::ZERO);
        } else {
            runtime
                .publish_result(&run, Outcome::Skipped, Duration::ZERO)
                .unwrap();
        }
        assert!(update.apply(&mut runtime, Duration::ZERO).is_empty());
        assert!(!publishing.await.unwrap());
        assert!(runtime.value_of(run.node()).is_none());
        assert!(!runtime.is_drained());
        assert_eq!(pool.available_permits(), 65536);
        runtime.complete(&run, Outcome::Produced(value()), Duration::ZERO);
        assert!(runtime.value_of(run.node()).is_none());
    }
}
