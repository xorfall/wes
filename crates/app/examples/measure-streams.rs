//! Synthetic release measurements through ordinary session/provider/storage boundaries.
//! No production shortcuts, live services, user workspace, or detached benchmark tasks.
use std::{
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use wes_adapters::{
    codec::Limits,
    journal::{Durability, FileHistory, ReadLimits},
    storage::TieredValues,
};
use wes_core::{
    Data, Primitive, Provenance, Shape, Value,
    capability::{Capability, Parameter, ProviderDescription, Safety},
};
use wes_engine::{
    calls::{CallJournal, RequiredPersistence},
    driver::CancellationToken,
    providers::{Call, InvocationFuture, Invoker},
    recording::{RecorderLimits, spawn_recorder},
    session::{self, RecordingMode, SessionStorage},
    source::SourceInput,
    storage::{AutoKeep, StoreWorkerLimits, spawn_store},
    streams::{StreamFuture, StreamSink, StreamingInvoker},
    type_sources::{TypeSourceError, TypeSourceReader},
    workspace::Workspace,
};

struct Case {
    name: &'static str,
    source: &'static str,
    branches: usize,
    offset: i64,
}
macro_rules! case {
    ($name:literal, $branches:literal, $offset:literal) => {
        Case {
            name: $name,
            source: include_str!(concat!(
                "../../../examples/stream-performance/",
                $name,
                ".wes"
            )),
            branches: $branches,
            offset: $offset,
        }
    };
}
const CASES: &[Case] = &[
    case!("native-only", 0, 0),
    case!("calc-only", 0, 1),
    case!("baseline", 1, 0),
    case!("native", 1, 0),
    case!("calc", 1, 1),
    case!("typed", 1, 1),
    case!("chain", 1, 4),
    case!("fork", 2, 0),
    case!("accumulate", 1, 0),
    case!("slow", 1, 0),
    case!("fork-slow", 2, 0),
];
struct DataPoints {
    start: Option<Instant>,
    source_unix_ns: i128,
    source_end: Option<Instant>,
    consumer_end: Option<Instant>,
    offered_at: Vec<Instant>,
    send_us: Vec<f64>,
    probe_us: Vec<Vec<f64>>,
    order: Vec<(usize, usize)>,
    max_offered_unfinished: usize,
}
struct Metrics {
    data: Mutex<DataPoints>,
    done: AtomicUsize,
    subscriptions: AtomicUsize,
    notify: Notify,
    count: usize,
    bytes: usize,
    branches: usize,
    offset: i64,
    delay: Duration,
}
struct Events(Arc<Metrics>);
impl StreamingInvoker for Events {
    fn subscribe(&self, _: Call, sink: StreamSink, _: CancellationToken) -> StreamFuture {
        let m = self.0.clone();
        m.subscriptions.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            sink.opened().unwrap();
            {
                let mut data = m.data.lock().unwrap();
                data.start = Some(Instant::now());
                data.source_unix_ns = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos() as i128;
            }
            for n in 0..m.count {
                let value = Value::new(
                    Shape::Unknown,
                    Data::Record(
                        [
                            ("seq".into(), Data::Int(n as i64)),
                            ("payload".into(), Data::Text("x".repeat(m.bytes).into())),
                        ]
                        .into(),
                    ),
                    Provenance::default(),
                )
                .unwrap();
                let offered = Instant::now();
                {
                    let mut data = m.data.lock().unwrap();
                    data.offered_at.push(offered);
                    data.max_offered_unfinished = data
                        .max_offered_unfinished
                        .max((n + 1).saturating_sub(m.done.load(Ordering::SeqCst)));
                }
                if sink.send(value).await.is_err() {
                    break;
                }
                m.data
                    .lock()
                    .unwrap()
                    .send_us
                    .push(offered.elapsed().as_secs_f64() * 1e6);
            }
            m.data.lock().unwrap().source_end = Some(Instant::now());
            m.notify.notify_one();
            Ok(())
        })
    }
}
struct NoFinite;
impl Invoker for NoFinite {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        panic!("stream port required")
    }
}
struct Probe(Arc<Metrics>);
fn sequence(data: &Data) -> i64 {
    match data {
        Data::Int(n) => *n,
        Data::Record(fields) => {
            if let Some(seq) = fields.get("seq") {
                sequence(seq)
            } else if let Some(Data::List(items)) = fields.get("items") {
                sequence(items.last().expect("nonempty accumulation"))
            } else {
                panic!("unexpected synthetic record")
            }
        }
        _ => panic!("unexpected synthetic value"),
    }
}
impl Invoker for Probe {
    fn invoke(&self, call: Call, cancel: CancellationToken) -> InvocationFuture {
        let m = self.0.clone();
        let n = (sequence(call.arguments["value"].data()) - m.offset) as usize;
        let branch = usize::from(call.capability.path[0] == "second");
        let slow = call.capability.path[0] == "slow";
        {
            let mut data = m.data.lock().unwrap();
            let latency = data.offered_at[n].elapsed().as_secs_f64() * 1e6;
            data.probe_us[branch].push(latency);
            data.order.push((n, branch));
        }
        Box::pin(async move {
            if slow {
                tokio::select! { _ = tokio::time::sleep(m.delay) => (), _ = cancel.cancelled() => () }
            }
            if branch + 1 == m.branches {
                let done = m.done.fetch_add(1, Ordering::SeqCst) + 1;
                if done == m.count {
                    m.data.lock().unwrap().consumer_end = Some(Instant::now());
                }
                m.notify.notify_one();
            }
            Ok(Value::new(
                Shape::Primitive(Primitive::Int),
                Data::Int(n as i64),
                Provenance::default(),
            )
            .unwrap())
        })
    }
}
struct NoFiles;
impl TypeSourceReader for NoFiles {
    fn read(&self, _: &str, _: usize) -> Result<String, TypeSourceError> {
        panic!("no live input files")
    }
}
fn summary(values: &[f64]) -> serde_json::Value {
    if values.is_empty() {
        return serde_json::Value::Null;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let percentile = |p: f64| {
        sorted[((sorted.len() as f64 * p).ceil() as usize)
            .saturating_sub(1)
            .min(sorted.len() - 1)]
    };
    serde_json::json!({"count":sorted.len(), "p50":percentile(0.5), "p95":percentile(0.95), "p99":percentile(0.99), "max":sorted.last().unwrap(), "sum":sorted.iter().sum::<f64>()})
}
fn bytes_under(path: &std::path::Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    std::fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                bytes_under(&entry.path())
            } else {
                meta.len()
            }
        })
        .sum()
}
#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(
        args.len(),
        6,
        "usage: measure-streams engine|storage|journal|durable CASE COUNT PAYLOAD_BYTES DELAY_US"
    );
    let mode = &args[1];
    assert!(["engine", "storage", "journal", "durable"].contains(&mode.as_str()));
    let case = CASES
        .iter()
        .find(|c| c.name == args[2])
        .expect("known case");
    let count: usize = args[3].parse().unwrap();
    let bytes: usize = args[4].parse().unwrap();
    let delay_us: u64 = args[5].parse().unwrap();
    assert!((1..=20_000).contains(&count) && bytes <= 16_384 && delay_us <= 10_000);
    assert!(
        case.branches != 0 || count <= 2000,
        "pure controls must fit the bounded execution-log verification window"
    );
    let root = tempfile::tempdir().unwrap();
    let m = Arc::new(Metrics {
        data: Mutex::new(DataPoints {
            start: None,
            source_unix_ns: 0,
            source_end: None,
            consumer_end: None,
            offered_at: Vec::with_capacity(count),
            send_us: Vec::with_capacity(count),
            probe_us: (0..case.branches)
                .map(|_| Vec::with_capacity(count))
                .collect(),
            order: Vec::with_capacity(count * case.branches),
            max_offered_unfinished: 0,
        }),
        done: AtomicUsize::new(0),
        subscriptions: AtomicUsize::new(0),
        notify: Notify::new(),
        count,
        bytes,
        branches: case.branches,
        offset: case.offset,
        delay: Duration::from_micros(delay_us),
    });
    let mut workspace = Workspace::new();
    let mut stream = Capability::new(["watch"], Shape::Unknown, Safety::Safe);
    stream.streaming = true;
    workspace
        .register_provider_ports(
            ProviderDescription::new("events", [stream], vec![]).unwrap(),
            Arc::new(NoFinite),
            Some(Arc::new(Events(m.clone()))),
        )
        .unwrap();
    let caps = ["take", "slow", "first", "second"].map(|name| {
        let mut cap = Capability::new([name], Shape::Primitive(Primitive::Int), Safety::Safe);
        cap.parameters = vec![Parameter::new("value", Shape::Unknown, true)];
        cap
    });
    workspace
        .register_provider(
            ProviderDescription::new("probe", caps, vec![]).unwrap(),
            Arc::new(Probe(m.clone())),
        )
        .unwrap();
    let recording = ["journal", "durable"].contains(&mode.as_str()).then(|| {
        spawn_recorder(
            FileHistory::open(
                &root.path().join("history"),
                ReadLimits::default(),
                Durability::File,
            )
            .unwrap(),
            RecorderLimits::default(),
        )
        .unwrap()
    });
    let storage = ["storage", "durable"].contains(&mode.as_str()).then(|| {
        spawn_store(
            TieredValues::open(
                &root.path().join("live"),
                &root.path().join("archive"),
                Limits::default(),
                Durability::File,
                None,
            )
            .unwrap(),
            StoreWorkerLimits::default(),
        )
        .unwrap()
    });
    let recording_mode = recording
        .as_ref()
        .map_or(RecordingMode::Ephemeral, |(recorder, _)| {
            RecordingMode::Required(CallJournal::new(
                recorder.clone(),
                RequiredPersistence::FileSynced,
            ))
        });
    let concurrency = NonZeroUsize::new(4).unwrap();
    let (handle, task) = if let Some((store, _)) = &storage {
        session::spawn_with_storage(
            workspace,
            recording_mode,
            Arc::new(NoFiles),
            concurrency,
            SessionStorage {
                worker: store.clone(),
                auto_keep: AutoKeep::default(),
            },
        )
        .unwrap()
    } else {
        session::spawn(workspace, recording_mode, Arc::new(NoFiles), concurrency).unwrap()
    };
    let wall = Instant::now();
    let accepted = handle
        .submit(SourceInput::new("measurement".into(), case.source.into()).unwrap())
        .await
        .unwrap();
    let timed_out = tokio::time::timeout(Duration::from_secs(60), async {
        while if case.branches == 0 {
            m.data.lock().unwrap().source_end.is_none()
        } else {
            m.done.load(Ordering::SeqCst) < count
        } {
            tokio::select! {
                _ = m.notify.notified() => (),
                _ = tokio::time::sleep(Duration::from_secs(2)) => {
                    if !handle.snapshot().await.unwrap().execution.errors.is_empty() { break; }
                }
            }
        }
        handle.wait_idle().await.unwrap();
    })
    .await
    .is_err();
    let elapsed = wall.elapsed();
    let snapshot = handle.snapshot().await.unwrap();
    let log = handle.log().await.unwrap();
    let values = handle.values().await.unwrap();
    let errors: Vec<_> = snapshot
        .execution
        .errors
        .values()
        .map(|e| serde_json::json!({"code":e.code(),"message":e.message()}))
        .collect();
    let state_bytes = bytes_under(root.path());
    let graph_nodes = snapshot.execution.graph.len();
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    if let Some((store, task)) = storage {
        store.shutdown().await.unwrap();
        task.join().await.unwrap();
    }
    if let Some((recorder, task)) = recording {
        recorder.shutdown().await.unwrap();
        task.join().await.unwrap();
    }
    let data = m.data.lock().unwrap();
    let sequence_ok = data.order
        == (0..count)
            .flat_map(|n| (0..case.branches).map(move |b| (n, b)))
            .collect::<Vec<_>>();
    let ready_runs: std::collections::HashSet<_> = log
        .entries
        .iter()
        .filter_map(|entry| {
            if let wes_engine::history::JournalEntry::Observed(e) = entry.entry()
                && Some(e.node()) == accepted.nodes.last()
                && e.state() == wes_engine::graph::NodeState::Ready
            {
                e.run()
            } else {
                None
            }
        })
        .collect();
    let latest_matches = accepted
        .nodes
        .last()
        .and_then(|node| snapshot.execution.values.get(node))
        .is_some_and(|v| v.data() == &Data::Int(count as i64 - 1 + case.offset));
    let validation_ok = if case.branches == 0 {
        ready_runs.len() == count && latest_matches
    } else {
        sequence_ok
    };
    let success = !timed_out
        && !accepted.nodes.is_empty()
        && errors.is_empty()
        && validation_ok
        && !snapshot.recording_blocked
        && log.unconfirmed == 0
        && log.capture_failures == 0
        && values.as_ref().is_none_or(|v| v.failures == 0)
        && data.send_us.len() == count
        && m.subscriptions.load(Ordering::SeqCst) == 1
        && (case.branches == 0 || data.max_offered_unfinished <= 501);
    let active = data
        .start
        .zip(data.consumer_end)
        .map(|(start, end)| end.duration_since(start).as_secs_f64());
    let producer = data
        .start
        .zip(data.source_end)
        .map(|(start, end)| end.duration_since(start).as_secs_f64());
    let settled = data
        .start
        .map(|start| (wall + elapsed).duration_since(start).as_secs_f64());
    // Existing log timestamps measure Ready capture, before durable acknowledgement. This
    // distinguishes computation completion from wait_idle without adding a provider probe.
    let ready_ns: Vec<i128> = log
        .entries
        .iter()
        .filter_map(|entry| {
            if let wes_engine::history::JournalEntry::Observed(e) = entry.entry()
                && Some(e.node()) == accepted.nodes.last()
                && e.state() == wes_engine::graph::NodeState::Ready
            {
                let at = e.at().parts();
                Some(at.seconds() as i128 * 1_000_000_000 + at.nanos() as i128)
            } else {
                None
            }
        })
        .collect();
    let ready_s = ready_ns
        .last()
        .map(|end| (*end - data.source_unix_ns) as f64 / 1e9)
        .filter(|s| *s > 0.0 && settled.is_some_and(|end| *s <= end + 0.005));
    // A retained suffix of the log cannot be paired with the first offered inputs.
    // Probe latencies remain complete even when old execution evidence was evicted.
    let ready_latency: Vec<f64> = ready_ns
        .iter()
        .zip(&data.offered_at)
        .map(|(end, offered)| {
            (*end - data.source_unix_ns) as f64 / 1000.0
                - offered.duration_since(data.start.unwrap()).as_secs_f64() * 1e6
        })
        .collect();
    let ready_latency_complete = ready_ns.len() == count && ready_latency.iter().all(|n| *n >= 0.0);
    let result = serde_json::json!({
        "schema":1,"ready_capture_ms":ready_s.map(|s|s*1000.0), "ready_capture_events_per_second":ready_s.map(|s|count as f64/s), "post_ready_drain_ms":ready_s.zip(settled).map(|(r,s)|((s-r)*1000.0).max(0.0)), "ready_latency_us":ready_latency_complete.then(|| summary(&ready_latency)),"mode":mode,"case":case.name,"count":count,"payload_bytes":bytes,"requested_delay_us":delay_us,
        "success":success,"timed_out":timed_out,"sequence_ok":(case.branches > 0).then_some(sequence_ok), "validation_ok":validation_ok, "ready_runs":ready_runs.len(), "latest_matches":latest_matches,"source_subscriptions":m.subscriptions.load(Ordering::SeqCst),
        "delivered":if case.branches == 0 {ready_runs.len()} else {m.done.load(Ordering::SeqCst)},"branches":case.branches,"graph_nodes":graph_nodes,
        "wall_ms":elapsed.as_secs_f64()*1000.0,"active_ms":active.map(|s|s*1000.0),"producer_ms":producer.map(|s|s*1000.0),
        "events_per_second":active.map(|s|count as f64/s), "settled_events_per_second":settled.map(|s|count as f64/s), "settled_ms":settled.map(|s|s*1000.0),"drain_ms":data.consumer_end.map(|end|(wall+elapsed).duration_since(end).as_secs_f64()*1000.0),
        "send_us":summary(&data.send_us),"probe_entry_us":data.probe_us.iter().map(|v|summary(v)).collect::<Vec<_>>(),
        "max_offered_unfinished":(case.branches > 0).then_some(data.max_offered_unfinished),"recording_blocked":snapshot.recording_blocked,
        "log_unconfirmed":log.unconfirmed,"log_capture_failures":log.capture_failures,"log_omitted":log.omitted,
        "storage_failures":values.map(|v|v.failures),"files_bytes_before_shutdown":state_bytes,"files_bytes_after_shutdown":bytes_under(root.path()),"errors":errors,
        "diagnostics":format!("{:?}",accepted.diagnostics),
    });
    println!("{result}");
    if !success {
        std::process::exit(1);
    }
}
