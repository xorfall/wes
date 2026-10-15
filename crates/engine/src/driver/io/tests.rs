use super::*;
use crate::{
    environments::InvocationAuthority,
    providers::Call,
    runtime::ExecutionTraits,
    source::SourceInput,
    streams::{
        self, ControlError, ControlFuture, ControlLimits, ControlOutcome, LiveController,
        StreamFuture, StreamHandle, StreamSink, StreamTask, StreamingInvoker,
    },
};
use std::sync::atomic::{AtomicBool, Ordering};
use wes_core::{
    Data, Primitive, Provenance, Shape, Value,
    capability::{Capability, Safety},
    contracts::ContractRegistry,
    flow::{FlowPolicy, ProviderReadOrigin},
};

fn value(n: i64) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(n),
        Default::default(),
    )
    .unwrap()
}
fn native() -> InvocationAuthority {
    InvocationAuthority::from_source(
        &SourceInput::new("synthetic".into(), "fixture".into()).unwrap(),
    )
}
fn runtime(joined: bool) -> (Runtime<()>, Run) {
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
        .find_map(|e| match e {
            Effect::Spawn(t) => Some(t.run),
            _ => None,
        })
        .unwrap();
    assert!(if joined {
        runtime.enter_joined(&run)
    } else {
        runtime.enter(&run)
    });
    (runtime, run)
}
struct NoWork;
impl Executor<()> for NoWork {
    fn execute(&self, _: RunTicket<()>, _: CancellationToken) -> super::super::ExecutionFuture {
        panic!("this fixture owns entry explicitly")
    }
}
fn io() -> ExecutionIo<()> {
    ExecutionIo::with_capacity(
        Arc::new(NoWork),
        super::super::ExecutionCapacity::new(std::num::NonZeroUsize::new(1).unwrap()).unwrap(),
    )
    .unwrap()
}
struct Entered {
    sink: StreamSink,
    finish: oneshot::Sender<()>,
}
struct Source(mpsc::UnboundedSender<Entered>);
impl StreamingInvoker for Source {
    fn subscribe(&self, _: Call, sink: StreamSink, _: CancellationToken) -> StreamFuture {
        sink.opened().unwrap();
        let (finish, receive) = oneshot::channel();
        self.0.send(Entered { sink, finish }).ok().unwrap();
        Box::pin(async move {
            receive.await.unwrap();
            Ok(())
        })
    }
}
struct Pending {
    action: Value,
    finish: oneshot::Sender<ControlOutcome>,
}
struct Controller(mpsc::UnboundedSender<Pending>);
impl LiveController for Controller {
    fn authorize(&self, principal: &InvocationAuthority, _: &Value) -> bool {
        principal.is_local_user()
    }
    fn invoke(&self, action: Value, _: CancellationToken) -> ControlFuture {
        let send = self.0.clone();
        Box::pin(async move {
            let (finish, receive) = oneshot::channel();
            send.send(Pending { action, finish }).ok().unwrap();
            receive.await.unwrap_or(ControlOutcome::Unknown)
        })
    }
}
struct SourceOwner {
    handle: StreamHandle,
    task: StreamTask,
    finish: oneshot::Sender<()>,
    controls: mpsc::UnboundedReceiver<Pending>,
}
impl SourceOwner {
    async fn new(
        run: Run,
        provenance: Provenance,
        guard: Option<crate::provider_reads::ReadGuard>,
    ) -> Self {
        let mut capability =
            Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Safe);
        capability.streaming = true;
        let (send, mut receive) = mpsc::unbounded_channel();
        let (handle, task) = streams::spawn_with_archives(
            Call {
                run: run.clone(),
                authority: native(),
                capability: Arc::new(capability),
                arguments: Default::default(),
            },
            Arc::new(Source(send)),
            provenance,
            Default::default(),
            CancellationToken::new(),
            None,
            None,
            vec![],
            guard,
        )
        .unwrap();
        let entered = receive.recv().await.unwrap();
        let contract = ContractRegistry::new().resolve("Int").unwrap();
        let (send, controls) = mpsc::unbounded_channel();
        entered
            .sink
            .install_controls(
                contract.clone(),
                contract,
                ControlLimits::default(),
                Arc::new(Controller(send)),
            )
            .unwrap();
        handle
            .wait_ready(&run, Duration::from_secs(5), CancellationToken::new())
            .await
            .unwrap();
        Self {
            handle,
            task,
            finish: entered.finish,
            controls,
        }
    }
    async fn join(self) {
        self.finish.send(()).unwrap();
        self.task.join().await.unwrap();
    }
}
async fn attach(
    io: &mut ExecutionIo<()>,
    runtime: &mut Runtime<()>,
    run: &Run,
    handle: &StreamHandle,
) -> bool {
    let reporter = super::super::publication::Reporter::new(
        run.clone(),
        io.publications.clone(),
        io.lifetime_credit.clone(),
        Arc::default(),
    )
    .with_sources(io.stream_updates.clone());
    let handle = handle.clone();
    let pending = tokio::spawn(async move { reporter.attach_source(&handle).await });
    let update = io.next().await;
    assert!(matches!(update, RuntimeInput::Stream(_)));
    assert!(io.apply_input(update, runtime).is_empty());
    pending.await.unwrap()
}

#[tokio::test]
async fn original_auxiliary_controls_survive_ordinary_publication_without_replacing_the_result() {
    let (mut runtime, run) = runtime(true);
    let mut io = io();
    let capture_policy = FlowPolicy::default().private();
    let mut owner = SourceOwner::new(
        run.clone(),
        Provenance::default().with_policy(&capture_policy),
        None,
    )
    .await;
    assert!(attach(&mut io, &mut runtime, &run, &owner.handle).await);
    assert!(!runtime.is_streaming(run.node()));
    let effects = runtime
        .publish_result(&run, Outcome::Produced(value(7)), Duration::ZERO)
        .unwrap();
    io.effects(effects);
    assert!(
        !runtime.accepts_live_io(&run),
        "other live I/O authority is not broadened"
    );
    let (external, provenance) = io.source_controls(&runtime, run.node(), native()).unwrap();
    assert_eq!(provenance.policy(), &capture_policy);
    let action = tokio::spawn(async move {
        external
            .invoke(value(1), Duration::from_secs(5), CancellationToken::new())
            .await
    });
    let pending = owner.controls.recv().await.unwrap();
    assert_eq!(pending.action.provenance().policy(), &capture_policy);
    assert!(
        pending
            .finish
            .send(ControlOutcome::Confirmed(value(2)))
            .is_ok()
    );
    assert_eq!(
        action.await.unwrap().unwrap().provenance().policy(),
        &capture_policy
    );
    assert_eq!(runtime.value_of(run.node()), Some(&value(7)));
    assert!(!runtime.is_drained());
    owner.join().await;
    io.source_handles.remove(&run);
    runtime.complete(&run, Outcome::Skipped, Duration::ZERO);
    assert!(runtime.is_drained());
    assert_eq!(runtime.value_of(run.node()), Some(&value(7)));
    assert!(io.source_controls(&runtime, run.node(), native()).is_err());
}

#[tokio::test]
async fn registration_refuses_wrong_run_duplicate_cancelled_finished_and_post_result_sources() {
    let (mut runtime, run) = runtime(true);
    let mut io = io();
    let (_, wrong) = self::runtime(true);
    let other = SourceOwner::new(wrong, Default::default(), None).await;
    assert!(!attach(&mut io, &mut runtime, &run, &other.handle).await);
    other.join().await;
    let cancelled = SourceOwner::new(run.clone(), Default::default(), None).await;
    cancelled.handle.cancel();
    assert!(!attach(&mut io, &mut runtime, &run, &cancelled.handle).await);
    cancelled.join().await;
    let ended = SourceOwner::new(run.clone(), Default::default(), None).await;
    let ended_handle = ended.handle.clone();
    ended.join().await;
    assert!(!attach(&mut io, &mut runtime, &run, &ended_handle).await);
    let owner = SourceOwner::new(run.clone(), Default::default(), None).await;
    assert!(attach(&mut io, &mut runtime, &run, &owner.handle).await);
    assert!(!attach(&mut io, &mut runtime, &run, &owner.handle).await);
    io.effects(
        runtime
            .publish_result(&run, Outcome::Produced(value(7)), Duration::ZERO)
            .unwrap(),
    );
    io.source_handles.remove(&run);
    assert!(!attach(&mut io, &mut runtime, &run, &owner.handle).await);
    owner.join().await;
}

#[tokio::test]
async fn abandoned_registration_and_ordinary_finite_entry_grant_no_auxiliary_authority() {
    let (mut runtime, run) = runtime(true);
    let mut io = io();
    let owner = SourceOwner::new(run.clone(), Default::default(), None).await;
    let reporter = super::super::publication::Reporter::new(
        run.clone(),
        io.publications.clone(),
        io.lifetime_credit.clone(),
        Arc::default(),
    )
    .with_sources(io.stream_updates.clone());
    let handle = owner.handle.clone();
    let pending = tokio::spawn(async move { reporter.attach_source(&handle).await });
    let update = io.next().await;
    pending.abort();
    let _ = pending.await;
    io.apply_input(update, &mut runtime);
    assert!(io.source_handles.is_empty());
    owner.join().await;
    let (mut finite, finite_run) = self::runtime(false);
    let owner = SourceOwner::new(finite_run.clone(), Default::default(), None).await;
    assert!(!attach(&mut io, &mut finite, &finite_run, &owner.handle).await);
    owner.join().await;
}

#[tokio::test]
async fn parent_cancellation_retires_external_controls_while_original_source_cleanup_remains_owned()
{
    for published in [false, true] {
        let (mut runtime, run) = runtime(true);
        let mut io = io();
        let business = CancellationToken::new();
        io.tokens.insert(run.clone(), business.clone());
        let mut owner = SourceOwner::new(run.clone(), Default::default(), None).await;
        assert!(attach(&mut io, &mut runtime, &run, &owner.handle).await);
        let original = owner.handle.live_controls(&run, native()).unwrap();
        let (external, _) = io.source_controls(&runtime, run.node(), native()).unwrap();
        if published {
            io.effects(
                runtime
                    .publish_result(&run, Outcome::Produced(value(7)), Duration::ZERO)
                    .unwrap(),
            );
        }
        io.effects(runtime.cancel(run.node(), Duration::ZERO));
        assert!(business.is_cancelled());
        assert!(!owner.handle.cancellation_token().is_cancelled());
        assert_eq!(
            external
                .invoke(value(1), Duration::from_secs(5), CancellationToken::new())
                .await,
            Err(ControlError::Changed)
        );
        let stop = tokio::spawn(async move {
            original
                .invoke(value(2), Duration::from_secs(5), CancellationToken::new())
                .await
        });
        let pending = owner.controls.recv().await.unwrap();
        assert!(
            pending
                .finish
                .send(ControlOutcome::Confirmed(value(3)))
                .is_ok()
        );
        stop.await.unwrap().unwrap();
        assert!(!runtime.is_drained());
        owner.join().await;
        runtime.complete(&run, Outcome::Skipped, Duration::ZERO);
        if published {
            assert_eq!(runtime.value_of(run.node()), Some(&value(7)));
        }
    }
}

#[tokio::test]
async fn capture_read_generation_loss_cancels_only_auxiliary_source_and_never_revives_its_port() {
    let (mut runtime, run) = runtime(true);
    let mut io = io();
    let business = CancellationToken::new();
    io.tokens.insert(run.clone(), business.clone());
    let allowed = Arc::new(AtomicBool::new(true));
    let guard = crate::provider_reads::ReadGuard::new({
        let allowed = allowed.clone();
        move || allowed.load(Ordering::Acquire)
    });
    let owner = SourceOwner::new(run.clone(), Default::default(), Some(guard)).await;
    assert!(attach(&mut io, &mut runtime, &run, &owner.handle).await);
    let (external, _) = io.source_controls(&runtime, run.node(), native()).unwrap();
    allowed.store(false, Ordering::Release);
    assert!(io.withdraw_source_reads(&Default::default()).is_empty());
    assert!(owner.handle.cancellation_token().is_cancelled());
    assert!(!business.is_cancelled());
    assert!(runtime.accepts_live_io(&run));
    allowed.store(true, Ordering::Release);
    assert_eq!(
        external
            .invoke(value(1), Duration::from_secs(5), CancellationToken::new())
            .await,
        Err(ControlError::Changed)
    );
    assert!(!attach(&mut io, &mut runtime, &run, &owner.handle).await);
    io.effects(
        runtime
            .publish_result(&run, Outcome::Produced(value(7)), Duration::ZERO)
            .unwrap(),
    );
    assert_eq!(runtime.value_of(run.node()), Some(&value(7)));
    owner.join().await;
    runtime.complete(&run, Outcome::Skipped, Duration::ZERO);
    assert_eq!(runtime.value_of(run.node()), Some(&value(7)));
}

#[tokio::test]
async fn missing_read_owner_blocks_attachment_without_cancelling_business() {
    let (mut runtime, run) = runtime(true);
    let mut io = io();
    let policy = FlowPolicy::default().with_provider_read(
        ProviderReadOrigin::new(
            "00000000-0000-4000-8000-000000000083".into(),
            "a".repeat(64),
            "b".repeat(64),
        )
        .unwrap(),
    );
    let owner = SourceOwner::new(
        run.clone(),
        Provenance::default().with_policy(&policy),
        None,
    )
    .await;
    assert!(!attach(&mut io, &mut runtime, &run, &owner.handle).await);
    assert!(io.source_handles.is_empty());
    assert!(runtime.accepts_live_io(&run));
    owner.join().await;
}

#[tokio::test]
async fn auxiliary_dataset_read_withdrawal_is_independent_and_primary_reads_still_cancel_the_node()
{
    let origin: wes_core::flow::DatasetReadOrigin = serde_json::from_value(serde_json::json!({ "store": "00000000-0000-4000-8000-000000000084", "dataset": "00000000-0000-4000-8000-000000000085" })).unwrap();
    let policy = FlowPolicy::default().with_dataset_read(origin.clone());
    for auxiliary in [true, false] {
        let (mut runtime, run) = runtime(auxiliary);
        let mut io = io();
        let business = CancellationToken::new();
        io.tokens.insert(run.clone(), business.clone());
        let owner = SourceOwner::new(
            run.clone(),
            Provenance::default().with_policy(&policy),
            None,
        )
        .await;
        if auxiliary {
            assert!(attach(&mut io, &mut runtime, &run, &owner.handle).await);
        } else {
            io.source_handles.insert(
                run.clone(),
                streaming::Registration {
                    handle: owner.handle.clone(),
                    auxiliary: false,
                    gate: Default::default(),
                },
            );
        }
        let (external, _) = io.source_controls(&runtime, run.node(), native()).unwrap();
        let access = crate::storage::datasets::DatasetAccess {
            withdrawn: [origin.dataset().to_owned()].into(),
            ..Default::default()
        };
        let nodes = io.withdraw_source_reads(&access);
        if auxiliary {
            assert!(nodes.is_empty());
            assert!(owner.handle.cancellation_token().is_cancelled());
            assert!(!business.is_cancelled());
        } else {
            assert_eq!(nodes, vec![run.node().clone()]);
            for node in nodes {
                io.effects(runtime.cancel(&node, Duration::ZERO));
            }
            assert!(business.is_cancelled());
        }
        assert_eq!(
            external
                .invoke(value(1), Duration::from_secs(5), CancellationToken::new())
                .await,
            Err(ControlError::Changed)
        );
        owner.join().await;
    }
}

#[tokio::test]
async fn auxiliary_control_binding_does_not_require_resident_business_data_or_replace_error_outcomes()
 {
    use crate::{
        history::Persistence,
        storage::{Retention, StoredOutput, StoredSnapshot, ValueHandle},
    };
    let handle = ValueHandle::fresh();
    let snapshot = StoredSnapshot::capture(
        handle.clone(),
        format!("sha256:{}", "a".repeat(64)),
        &value(7),
    )
    .unwrap();
    let stored = Outcome::Stored(StoredOutput {
        handle,
        snapshot: Some(snapshot),
        bytes: 512,
        kept: false,
        retention: Retention::Automatic,
        retained_persistence: Persistence::Volatile,
        requires_ownership_record: false,
    });
    let failed = Outcome::Failed(RuntimeCode::ExecutionFailed.error("Synthetic failure", None));
    let unknown =
        Outcome::Failed(RuntimeCode::ExternalOutcomeUnknown.error("Synthetic unknown", None));
    for outcome in [stored, failed, unknown] {
        let (mut runtime, run) = runtime(true);
        let mut io = io();
        let owner = SourceOwner::new(run.clone(), Default::default(), None).await;
        assert!(attach(&mut io, &mut runtime, &run, &owner.handle).await);
        io.effects(
            runtime
                .publish_result(&run, outcome.clone(), Duration::ZERO)
                .unwrap(),
        );
        let (external, _) = io.source_controls(&runtime, run.node(), native()).unwrap();
        assert!(external.description().is_ok());
        assert!(runtime.value_of(run.node()).is_none());
        owner.join().await;
        io.source_handles.remove(&run);
        runtime.complete(&run, Outcome::Produced(value(99)), Duration::ZERO);
        match outcome {
            Outcome::Stored(output) => assert_eq!(
                runtime.stored_output(run.node()).unwrap().snapshot,
                output.snapshot.unwrap()
            ),
            Outcome::Failed(error) => assert_eq!(runtime.error_of(run.node()).unwrap(), &error),
            _ => unreachable!(),
        }
        assert!(matches!(external.description(), Err(ControlError::Changed)));
    }
}

#[tokio::test]
async fn invalidation_deletion_and_shutdown_retire_existing_ports_without_detaching_cleanup() {
    for operation in ["invalidate", "drop", "close"] {
        let (mut runtime, run) = runtime(true);
        let mut io = io();
        let owner = SourceOwner::new(run.clone(), Default::default(), None).await;
        assert!(attach(&mut io, &mut runtime, &run, &owner.handle).await);
        io.effects(
            runtime
                .publish_result(&run, Outcome::Produced(value(7)), Duration::ZERO)
                .unwrap(),
        );
        let (external, _) = io.source_controls(&runtime, run.node(), native()).unwrap();
        let effects = match operation {
            "invalidate" => runtime.invalidate(run.node(), Duration::ZERO).unwrap(),
            "drop" => runtime.drop_node(run.node()).unwrap().1,
            _ => runtime.close(),
        };
        io.effects(effects);
        assert!(!runtime.accepts_auxiliary_source(&run));
        assert_eq!(
            external
                .invoke(value(1), Duration::from_secs(5), CancellationToken::new())
                .await,
            Err(ControlError::Changed)
        );
        assert!(!owner.handle.cancellation_token().is_cancelled());
        assert!(!runtime.is_drained());
        owner.join().await;
        runtime.complete(&run, Outcome::Skipped, Duration::ZERO);
        assert!(runtime.is_drained());
    }
}

struct RepairSink {
    entered: Option<oneshot::Sender<()>>,
    release: std::sync::mpsc::Receiver<()>,
}
impl crate::history::JournalSink for RepairSink {
    fn append(
        &mut self,
        record: &crate::history::Record,
    ) -> Result<crate::history::AppendReceipt, crate::history::RecordError> {
        assert!(matches!(
            record,
            crate::history::Record::Recovery(crate::history::RecoveryEntry::Accepted { .. })
        ));
        self.entered.take().unwrap().send(()).unwrap();
        self.release.recv_timeout(Duration::from_secs(5)).unwrap();
        Ok(crate::history::AppendReceipt {
            persistence: crate::history::Persistence::FileSynced,
            end_offset: 1,
        })
    }
}

#[tokio::test]
async fn bind_and_send_recheck_retired_parent_after_acknowledged_admission_repair() {
    use crate::{
        history::{AppendReceipt, CommandRecord, HistoryCheckpoint, Persistence},
        plan::{Input, MetaTask},
        tasks::source_control::{BoundSourceControl, Captured},
    };
    use wes_language::{Span, vocabulary::MetaCommand};
    for send_action in [false, true] {
        let (mut runtime, run) = runtime(true);
        let mut io = io();
        let mut owner = SourceOwner::new(run.clone(), Default::default(), None).await;
        assert!(attach(&mut io, &mut runtime, &run, &owner.handle).await);
        let (external, provenance) = io.source_controls(&runtime, run.node(), native()).unwrap();
        let (_, binding_run) = self::runtime(false);
        let command = CommandRecord {
            source_name: "fixture.wes".into(),
            source_start: wes_language::Position { line: 1, column: 1 },
            changed_nodes: vec![],
            document: None,
            revision_of: None,
            environments: None,
            cell: "synthetic-control".into(),
            text: ":source bind $events > controls".into(),
            nodes: vec![binding_run.node().clone()],
            type_sources: Default::default(),
            calculation_package: None,
            imports: vec![],
            replay: ":source bind $events > controls".into(),
        };
        let (entered, waiting) = oneshot::channel();
        let (release, receive) = std::sync::mpsc::channel();
        let (recorder, writer) = crate::recording::spawn_recorder(
            RepairSink {
                entered: Some(entered),
                release: receive,
            },
            Default::default(),
        )
        .unwrap();
        let journal = crate::calls::CallJournal::new(
            recorder.clone(),
            crate::calls::RequiredPersistence::FileSynced,
        );
        let checkpoint = AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: 0,
        };
        let admission = journal
            .recover(
                &command,
                HistoryCheckpoint {
                    journal: checkpoint.clone(),
                    recovery: checkpoint,
                },
                false,
            )
            .unwrap();
        let mut task = BoundSourceControl::bind(
            MetaTask {
                spec: (if send_action {
                    MetaCommand::SourceSend
                } else {
                    MetaCommand::SourceBind
                })
                .spec(&[]),
                target: None,
                tail: vec![],
                subjects: vec![Input::FromNode(crate::graph::OutputRef::data(
                    run.node().clone(),
                ))],
                inputs: (if send_action {
                    vec![("action".into(), Input::Literal(value(1)))]
                } else {
                    vec![]
                })
                .into_iter()
                .collect(),
                guards: Default::default(),
            },
            Span::new(0, 1).unwrap(),
        )
        .unwrap();
        task.policy = provenance.policy().clone();
        task.admission = Some(admission);
        task.captured = Some(Ok(if send_action {
            Captured::Action {
                port: external,
                action: value(1),
                budget: Duration::from_secs(5),
            }
        } else {
            Captured::Binding {
                value: Value::management(
                    wes_core::MetaType::SourceControls,
                    Data::Record(Default::default()),
                    "synthetic-binding".into(),
                )
                .with_provenance(provenance),
                port: external,
            }
        }));
        let pending =
            tokio::spawn(task.execute(binding_run, Some(journal), CancellationToken::new()));
        waiting.await.unwrap();
        io.effects(runtime.cancel(run.node(), Duration::ZERO));
        assert!(
            !pending.is_finished(),
            "accepted repair remains owned until its receipt"
        );
        release.send(()).unwrap();
        let report = pending.await.unwrap();
        assert!(matches!(report.outcome, Outcome::Failed(_)));
        assert!(owner.controls.try_recv().is_err());
        owner.join().await;
        recorder.shutdown().await.unwrap();
        writer.join().await.unwrap();
    }
}
