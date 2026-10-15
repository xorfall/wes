use super::*;
use crate::{
    calls::{CallJournal, RequiredPersistence},
    history::{
        AppendReceipt, CommandRecord, JournalSink, Persistence, Record, RecordError, RecoveryEntry,
    },
    recording::{RecorderLimits, spawn_recorder},
};
use std::sync::Mutex;

struct Sink {
    records: Arc<Mutex<Vec<Record>>>,
    calling: Option<(oneshot::Sender<()>, std::sync::mpsc::Receiver<()>)>,
}
impl JournalSink for Sink {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        if matches!(record, Record::Recovery(RecoveryEntry::Calling(_)))
            && let Some((entered, release)) = self.calling.take()
        {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        let mut records = self.records.lock().unwrap();
        records.push(record.clone());
        Ok(AppendReceipt {
            persistence: Persistence::Volatile,
            end_offset: records.len() as u64,
        })
    }
}
fn ticket(task: BoundTask) -> (Runtime<BoundTask>, RunTicket<BoundTask>) {
    let mut runtime = Runtime::new();
    runtime.add(task.clone(), [], task.traits()).unwrap();
    let ticket = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|effect| match effect {
            crate::runtime::Effect::Spawn(ticket) => Some(ticket),
            _ => None,
        })
        .unwrap();
    assert!(runtime.enter_joined(&ticket.run));
    (runtime, ticket)
}
fn command(node: NodeId) -> CommandRecord {
    CommandRecord {
        source_name: "synthetic.wes".into(),
        source_start: wes_language::Position { line: 1, column: 1 },
        document: None,
        revision_of: None,
        environments: None,
        cell: "synthetic-companion".into(),
        text: "business work number:7".into(),
        nodes: vec![node],
        changed_nodes: vec![],
        type_sources: Default::default(),
        calculation_package: None,
        imports: vec![],
        replay: "business work number:7".into(),
    }
}

#[tokio::test]
async fn capture_loss_during_acknowledged_calling_write_refuses_business_entry_without_replay() {
    let mut f = Fixture::new(false, false);
    let (mut runtime, mut ticket) = ticket(f.task(Duration::ZERO));
    let run = ticket.run.clone();
    let records = Arc::new(Mutex::new(Vec::new()));
    let (calling, wait_calling) = oneshot::channel();
    let (release, held) = std::sync::mpsc::channel();
    let (recorder, writer) = spawn_recorder(
        Sink {
            records: records.clone(),
            calling: Some((calling, held)),
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let journal = CallJournal::new(recorder.clone(), RequiredPersistence::Volatile);
    ticket.payload = ticket
        .payload
        .with_admission(journal.admit(command(run.node().clone())).await.unwrap());
    let owner = Owner::default();
    let (publications, mut results) = mpsc::channel(1);
    let (updates, mut sources) = mpsc::channel(1);
    let reporter = Reporter::new(
        run.clone(),
        publications,
        Arc::new(tokio::sync::Semaphore::new(65536)),
        Arc::new(Mutex::new(None)),
    )
    .with_sources(updates);
    let joined = tokio::spawn(
        TaskExecutor::recorded(journal)
            .with_recordings(owner.clone())
            .execute_joined(
                ticket,
                CancellationToken::new(),
                driver::progress::Reporter::silent(),
                reporter,
            ),
    );
    let source = next(&mut f.sources).await;
    let mut registrations = Default::default();
    sources
        .recv()
        .await
        .unwrap()
        .register(&runtime, &mut registrations);
    f.ready(&source);
    wait_calling.await.unwrap();
    assert_eq!(f.business_calls.load(Ordering::SeqCst), 0);
    let source_handle = owner.source(run.node(), run.id().as_str()).unwrap();
    let mut changes = source_handle.subscribe();
    source
        .finish
        .send(Err(InvocationError::Failed(
            RuntimeCode::ExecutionFailed.error("synthetic source closed", None),
        )))
        .unwrap();
    while !matches!(changes.borrow_and_update().phase, Phase::Failed(_)) {
        changes.changed().await.unwrap();
    }
    release.send(()).unwrap();
    let publication = results.recv().await.unwrap();
    publication.apply(&mut runtime, Duration::ZERO);
    let report = joined.await.unwrap();
    assert!(matches!(report.outcome, Outcome::Failed(_)));
    runtime.complete(&run, report.outcome, Duration::ZERO);
    assert_eq!(f.source_calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.business_calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.controller.calls.load(Ordering::SeqCst), 0);
    assert!(owner.source(run.node(), run.id().as_str()).is_err());
    let records = records.lock().unwrap();
    let calling = records
        .iter()
        .filter_map(|record| match record {
            Record::Recovery(RecoveryEntry::Calling(call)) => Some(call),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(calling.len(), 1);
    assert_eq!(calling[0].run, *run.id());
    assert_eq!(calling[0].capability, "business work");
    assert_eq!(records.iter().filter(|record| matches!(record, Record::Recovery(RecoveryEntry::Called { run: id, .. }) if id == run.id())).count(), 1);
    drop(records);
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[tokio::test]
async fn command_receipt_from_another_journal_refuses_before_any_capture_or_business_entry() {
    let f = Fixture::new(false, false);
    let (_, mut ticket) = ticket(f.task(Duration::ZERO));
    let (foreign, foreign_writer) = spawn_recorder(
        Sink {
            records: Default::default(),
            calling: None,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let foreign_journal = CallJournal::new(foreign.clone(), RequiredPersistence::Volatile);
    ticket.payload = ticket.payload.with_admission(
        foreign_journal
            .admit(command(ticket.run.node().clone()))
            .await
            .unwrap(),
    );
    let (recorder, writer) = spawn_recorder(
        Sink {
            records: Default::default(),
            calling: None,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let journal = CallJournal::new(recorder.clone(), RequiredPersistence::Volatile);
    let report = TaskExecutor::recorded(journal)
        .execute_joined(
            ticket,
            CancellationToken::new(),
            driver::progress::Reporter::silent(),
            Reporter::silent(),
        )
        .await;
    assert!(matches!(report.outcome, Outcome::Failed(_)));
    assert_eq!(f.source_calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.business_calls.load(Ordering::SeqCst), 0);
    foreign.shutdown().await.unwrap();
    foreign_writer.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}
