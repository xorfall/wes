use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex},
    time::Duration,
};
use wes_core::{
    Data, Shape,
    capability::{Capability, ProviderDescription, Safety},
};
use wes_engine::{
    calls::{CallJournal, RequiredPersistence},
    driver::CancellationToken,
    history::{AppendReceipt, JournalEntry, JournalSink, Persistence, Record, RecordError},
    providers::{Call, InvocationFuture, Invoker},
    recording::{RecorderLimits, spawn_recorder},
    session::{self, RecordingMode},
    source::SourceInput,
    trace::{TraceSink, text},
    type_sources::{TypeSourceError, TypeSourceReader},
    workspace::Workspace,
};
struct NoFiles;
impl TypeSourceReader for NoFiles {
    fn read(&self, _: &str, _: usize) -> Result<String, TypeSourceError> {
        panic!("no files")
    }
}
struct Observed;

#[tokio::test]
async fn typed_repeat_keeps_prior_trace_addressable_by_exact_run() {
    let mut workspace =
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    workspace
        .register_provider(
            ProviderDescription::new(
                "fixture",
                [Capability::new(["read"], Shape::Unknown, Safety::Safe)],
                vec![],
            )
            .unwrap(),
            Arc::new(Observed),
        )
        .unwrap();
    let (session, task) = session::spawn(
        workspace,
        RecordingMode::Ephemeral,
        Arc::new(NoFiles),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let source = "@trace(binary) fixture read";
    let first = session
        .submit(SourceInput::new("original".into(), source.into()).unwrap())
        .await
        .unwrap();
    session.wait_idle().await.unwrap();
    let node = &first.nodes[0];
    let before = session.observe().await.unwrap();
    let old_run = before.state.execution.runs[node].clone();
    let old_trace = before.traces.get(node, Some(old_run.as_str())).unwrap();
    let repeated = session
        .submit(
            SourceInput::new("repeat".into(), source.into())
                .unwrap()
                .with_repeat("original".into(), false)
                .unwrap(),
        )
        .await
        .unwrap();
    session.wait_idle().await.unwrap();
    let after = session.observe().await.unwrap();
    assert_ne!(repeated.repeated_run.as_ref().unwrap(), &old_run);
    assert_eq!(
        after
            .traces
            .get(node, Some(old_run.as_str()))
            .unwrap()
            .data(),
        old_trace.data()
    );
    assert!(
        after
            .traces
            .get(node, repeated.repeated_run.as_ref().map(|run| run.as_str()))
            .is_some()
    );
    assert_eq!(after.state.execution.graph.len(), 1);
    session.shutdown().await.unwrap();
    task.join().await.unwrap();
}
impl Invoker for Observed {
    fn supports_trace(&self, profile: &str) -> bool {
        profile == "binary"
    }
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        Box::pin(async { Ok(text("business result")) })
    }
    fn invoke_observed(
        &self,
        call: Call,
        cancel: CancellationToken,
        trace: TraceSink,
    ) -> InvocationFuture {
        trace.emit("synthetic", text("evidence"));
        self.invoke(call, cancel)
    }
}
struct Sink {
    records: Arc<Mutex<Vec<Record>>>,
    fail_trace: bool,
}
impl JournalSink for Sink {
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        if self.fail_trace && matches!(record, Record::Journal(JournalEntry::Trace(_))) {
            return Err(RecordError::backend(
                "fixture",
                false,
                std::io::Error::other("secret recorder canary"),
            ));
        }
        self.records.lock().unwrap().push(record.clone());
        Ok(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: 1,
        })
    }
}
#[tokio::test]
async fn failed_trace_ack_preserves_result_and_reports_non_durable_evidence() {
    let mut workspace =
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    workspace
        .register_provider(
            ProviderDescription::new(
                "fixture",
                [Capability::new(["read"], Shape::Unknown, Safety::Safe)],
                vec![],
            )
            .unwrap(),
            Arc::new(Observed),
        )
        .unwrap();
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, writer) = spawn_recorder(
        Sink {
            records: records.clone(),
            fail_trace: true,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (session, task) = session::spawn(
        workspace,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        Arc::new(NoFiles),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let reply = session
        .submit(
            SourceInput::new(
                "source".into(),
                "@trace(binary) fixture read > answer".into(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let node = &reply.nodes[0];
    tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let observation = session.observe().await.unwrap();
    assert_eq!(
        observation.state.execution.values[node].data(),
        &Data::Text("business result".into())
    );
    let value = observation.traces.get(node, None).unwrap();
    let Data::Record(trace) = value.data() else {
        panic!()
    };
    assert_eq!(trace["profile"], Data::Text("binary".into()));
    assert_eq!(trace["persistence"], Data::Text("failed".into()));
    assert_eq!(trace["state"], Data::Text("completed".into()));
    assert!(observation.log.entries.iter().any(|entry| matches!(entry.entry(),JournalEntry::Noticed(record) if record.error().message() == "Inspection completed, but its trace could not be recorded.")));
    assert!(!format!("{:?}", observation.log).contains("secret recorder canary"));
    assert!(
        !records
            .lock()
            .unwrap()
            .iter()
            .any(|r| matches!(r, Record::Journal(JournalEntry::Trace(_))))
    );
    session.shutdown().await.unwrap();
    task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}

#[derive(Clone, Default)]
struct MultipleProfiles(Arc<Mutex<Vec<Option<String>>>>);
impl Invoker for MultipleProfiles {
    fn supports_trace(&self, profile: &str) -> bool {
        matches!(profile, "binary" | "grpc")
    }
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        self.0.lock().unwrap().push(None);
        Box::pin(async { Ok(text("plain result")) })
    }
    fn invoke_observed(&self, _: Call, _: CancellationToken, trace: TraceSink) -> InvocationFuture {
        let profile = trace.profile();
        self.0.lock().unwrap().push(Some(profile.clone()));
        trace.emit("selected.profile", text(&profile));
        Box::pin(async move { Ok(text(profile)) })
    }
}
fn profiled_workspace(provider: &MultipleProfiles) -> Workspace {
    let mut workspace =
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    workspace
        .register_provider(
            ProviderDescription::new(
                "fixture",
                [Capability::new(["read"], Shape::Unknown, Safety::Safe)],
                vec![],
            )
            .unwrap(),
            Arc::new(provider.clone()),
        )
        .unwrap();
    workspace
}
#[tokio::test]
async fn explicit_profiles_reach_adapter_query_journal_and_inert_session_restore() {
    use wes_engine::history::{HistoryCapture, HistoryCaptureLimits, HistoryCheckpoint};
    let provider = MultipleProfiles::default();
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, writer) = spawn_recorder(
        Sink {
            records: records.clone(),
            fail_trace: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (session, task) = session::spawn(
        profiled_workspace(&provider),
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        Arc::new(NoFiles),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let mut expected = Vec::new();
    for (cell, source, profile) in [
        (
            "binary",
            "@trace(binary) fixture read > binaryResult",
            Some("binary"),
        ),
        (
            "grpc",
            "@trace(grpc) fixture read > grpcResult",
            Some("grpc"),
        ),
        ("plain", "fixture read > plainResult", None),
    ] {
        let reply = session
            .submit(SourceInput::new(cell.into(), source.into()).unwrap())
            .await
            .unwrap();
        assert_eq!(reply.nodes.len(), 1, "{reply:?}");
        tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
            .await
            .unwrap()
            .unwrap();
        let observation = session.observe().await.unwrap();
        let node = reply.nodes[0].clone();
        if let Some(profile) = profile {
            let value = observation.traces.get(&node, None).unwrap();
            let Data::Record(fields) = value.data() else {
                panic!()
            };
            assert_eq!(fields["profile"], Data::Text(profile.into()));
            assert_eq!(fields["persistence"], Data::Text("recorded".into()));
            assert_eq!(
                observation.state.execution.values[&node].data(),
                &Data::Text(profile.into())
            );
            expected.push((node, value));
        } else {
            assert!(observation.traces.get(&node, None).is_none());
        }
    }
    let query = session
        .submit(
            SourceInput::new("query".into(), ":read trace:$grpcResult > evidence".into()).unwrap(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        session.snapshot().await.unwrap().execution.values[&query.nodes[0]].data(),
        expected[1].1.data()
    );
    let rejected = session
        .submit(SourceInput::new("unsupported".into(), "@trace(http) fixture read".into()).unwrap())
        .await
        .unwrap();
    assert!(rejected.nodes.is_empty());
    assert!(
        rejected
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.code == "ENG006" && !d.message.contains("HTTP"))
    );
    assert_eq!(
        *provider.0.lock().unwrap(),
        [Some("binary".into()), Some("grpc".into()), None]
    );
    session.shutdown().await.unwrap();
    task.join().await.unwrap();
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    let mut capture = HistoryCapture::new(HistoryCaptureLimits::default());
    let records = records.lock().unwrap().clone();
    let traces: Vec<_> = records
        .iter()
        .filter_map(|r| {
            if let Record::Journal(JournalEntry::Trace(trace)) = r {
                Some(trace)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(traces.len(), 2);
    for (trace, (_, expected)) in traces.iter().zip(&expected) {
        assert_eq!(&trace.value, expected);
    }
    for record in records {
        capture.push(record).unwrap();
    }
    let receipt = AppendReceipt {
        persistence: Persistence::FileSynced,
        end_offset: 1,
    };
    let history = capture.finish(HistoryCheckpoint {
        journal: receipt,
        recovery: receipt,
    });
    let restored = session::restore(
        profiled_workspace(&provider),
        RecordingMode::Ephemeral,
        None,
        history,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let (session, task) = restored
        .spawn(Arc::new(NoFiles), NonZeroUsize::new(2).unwrap())
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), session.wait_idle())
        .await
        .unwrap()
        .unwrap();
    let observation = session.observe().await.unwrap();
    for (node, expected) in expected {
        assert_eq!(observation.traces.get(&node, None), Some(expected));
    }
    assert_eq!(
        provider.0.lock().unwrap().len(),
        3,
        "restore cannot invoke a provider"
    );
    session.shutdown().await.unwrap();
    task.join().await.unwrap();
}
