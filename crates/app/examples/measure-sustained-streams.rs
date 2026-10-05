//! Synthetic release measurements through ordinary session/provider/storage boundaries.
//! No production shortcuts, live services, user workspace, or detached benchmark tasks.
use std::{
    num::NonZeroUsize,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
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
    Case {
        name: "chain-single-calc",
        source: include_str!(
            "../../../examples/sustained-stream-performance/chain-single-calc.wes"
        ),
        branches: 1,
        offset: 4,
    },
    case!("fork", 2, 0),
    case!("accumulate", 1, 0),
    case!("slow", 1, 0),
    case!("fork-slow", 2, 0),
];
// Fixed-size histograms report bucket upper bounds, not exact percentiles.
#[derive(Clone)]
struct Histogram {
    buckets: [u64; 64],
    count: u64,
    max_us: f64,
    sum_us: f64,
}
impl Default for Histogram {
    fn default() -> Self {
        Self {
            buckets: [0; 64],
            count: 0,
            max_us: 0.0,
            sum_us: 0.0,
        }
    }
}
impl Histogram {
    fn add(&mut self, duration: Duration) {
        let us = duration.as_secs_f64() * 1e6;
        let bucket = (64 - (us.ceil() as u64).max(1).leading_zeros()) as usize;
        self.buckets[bucket.min(63)] += 1;
        self.count += 1;
        self.max_us = self.max_us.max(us);
        self.sum_us += us;
    }
    fn json(&self) -> serde_json::Value {
        let percentile = |p: f64| {
            let target = (self.count as f64 * p).ceil() as u64;
            let mut total = 0;
            for (i, n) in self.buckets.iter().enumerate() {
                total += n;
                if total >= target {
                    return (2u64.pow(i as u32) - 1) as f64;
                }
            }
            0.0
        };
        serde_json::json!({"count":self.count,"p50_upper_us":percentile(0.5),
            "p95_upper_us":percentile(0.95),"p99_upper_us":percentile(0.99),
            "max_us":self.max_us,"sum_us":self.sum_us})
    }
}
struct DataPoints {
    start: Option<Instant>,
    source_end: Option<Instant>,
    consumer_end: Option<Instant>,
    offered: std::collections::VecDeque<(usize, Instant)>,
    sends: Histogram,
    probes: Vec<Histogram>,
    next_event: usize,
    next_branch: usize,
    sequence_ok: bool,
    max_offered_unfinished: usize,
    cancellation_timestamps_omitted: usize,
}
struct Metrics {
    data: Mutex<DataPoints>,
    stopping: AtomicBool,
    done: AtomicUsize,
    sent: AtomicUsize,
    subscriptions: AtomicUsize,
    notify: Notify,
    duration: Duration,
    bytes: usize,
    branches: usize,
    offset: i64,
    delay: Duration,
}
struct Events(Arc<Metrics>);
impl StreamingInvoker for Events {
    fn subscribe(&self, _: Call, sink: StreamSink, cancel: CancellationToken) -> StreamFuture {
        let m = self.0.clone();
        m.subscriptions.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            sink.opened().unwrap();
            let start = Instant::now();
            m.data.lock().unwrap().start = Some(start);
            for n in 0..20_000_000 {
                if start.elapsed() >= m.duration || cancel.is_cancelled() {
                    break;
                }
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
                if m.branches > 0 {
                    let mut data = m.data.lock().unwrap();
                    if !m.stopping.load(Ordering::SeqCst) {
                        data.max_offered_unfinished =
                            data.max_offered_unfinished.max(data.offered.len() + 1);
                    }
                    if data.offered.len() < 501 {
                        data.offered.push_back((n, offered));
                    } else if m.stopping.load(Ordering::SeqCst) {
                        // Cancellation may release queued credits before the source joins.
                        // Those abandoned inputs need no probe timestamp; keep telemetry bounded.
                        data.cancellation_timestamps_omitted += 1;
                    } else {
                        data.sequence_ok = false;
                    }
                }
                if sink.send(value).await.is_err() {
                    break;
                }
                m.sent.fetch_add(1, Ordering::SeqCst);
                m.data.lock().unwrap().sends.add(offered.elapsed());
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
            let offered_input = data.offered.front().copied();
            let (expected, offered) = offered_input.unwrap_or((usize::MAX, Instant::now()));
            let valid = expected == n && data.next_event == n && data.next_branch == branch;
            data.sequence_ok &= valid;
            data.probes[branch].add(offered.elapsed());
            data.next_branch += 1;
            if data.next_branch == m.branches {
                data.next_branch = 0;
                data.next_event += 1;
            }
        }
        Box::pin(async move {
            if slow {
                tokio::select! { _ = tokio::time::sleep(m.delay) => (), _ = cancel.cancelled() => () }
            }
            if branch + 1 == m.branches {
                let mut data = m.data.lock().unwrap();
                data.offered.pop_front();
                data.consumer_end = Some(Instant::now());
                m.done.fetch_add(1, Ordering::SeqCst);
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
        7,
        "usage: measure-sustained-streams engine|storage|journal|durable CASE SECONDS PAYLOAD_BYTES DELAY_US complete|cancel"
    );
    let mode = &args[1];
    assert!(["engine", "storage", "journal", "durable"].contains(&mode.as_str()));
    let case = CASES
        .iter()
        .find(|c| c.name == args[2])
        .expect("known case");
    let seconds: u64 = args[3].parse().unwrap();
    let bytes: usize = args[4].parse().unwrap();
    let delay_us: u64 = args[5].parse().unwrap();
    let cancel_run = args[6] == "cancel";
    assert!(["complete", "cancel"].contains(&args[6].as_str()));
    assert!((1..=120).contains(&seconds) && bytes <= 16_384 && delay_us <= 10_000);
    let root = tempfile::tempdir().unwrap();
    let m = Arc::new(Metrics {
        data: Mutex::new(DataPoints {
            start: None,
            source_end: None,
            consumer_end: None,
            offered: std::collections::VecDeque::with_capacity(501),
            sends: Histogram::default(),
            probes: vec![Histogram::default(); case.branches],
            next_event: 0,
            next_branch: 0,
            sequence_ok: true,
            max_offered_unfinished: 0,
            cancellation_timestamps_omitted: 0,
        }),
        stopping: AtomicBool::new(false),
        done: AtomicUsize::new(0),
        sent: AtomicUsize::new(0),
        subscriptions: AtomicUsize::new(0),
        notify: Notify::new(),
        duration: Duration::from_secs(seconds * if cancel_run { 2 } else { 1 }),
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
    let mut heartbeats = Vec::with_capacity(180);
    let mut cancel_ms = None;
    let timed_out = tokio::time::timeout(Duration::from_secs(seconds + 45), async {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        loop {
            if m.data.lock().unwrap().source_end.is_some() { break; }
            tokio::select! {
                _ = m.notify.notified() => (),
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    let at = Instant::now();
                    let snapshot = handle.snapshot().await.unwrap();
                    heartbeats.push(serde_json::json!({"elapsed_s":wall.elapsed().as_secs_f64(),
                        "snapshot_ms":at.elapsed().as_secs_f64()*1000.0,
                        "sent":m.sent.load(Ordering::SeqCst), "completed":m.done.load(Ordering::SeqCst),
                        "recording_blocked":snapshot.recording_blocked,
                        "errors":snapshot.execution.errors.len()}));
                    if !snapshot.execution.errors.is_empty() { break; }
                }
            }
            if cancel_run && Instant::now() >= deadline {
                m.stopping.store(true, Ordering::SeqCst);
                let at = Instant::now();
                handle.cancel(accepted.nodes[0].clone()).await.unwrap();
                handle.wait_idle().await.unwrap();
                cancel_ms = Some(at.elapsed().as_secs_f64()*1000.0);
                break;
            }
        }
        handle.wait_idle().await.unwrap();
    }).await.is_err();
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
    let count = m.sent.load(Ordering::SeqCst);
    let done = m.done.load(Ordering::SeqCst);
    let latest_matches = accepted
        .nodes
        .last()
        .and_then(|node| snapshot.execution.values.get(node))
        .is_some_and(|v| v.data() == &Data::Int(count as i64 - 1 + case.offset));
    let validation_ok = if cancel_run {
        cancel_ms.is_some()
            && data.sequence_ok
            && done <= count
            && data.max_offered_unfinished <= 501
    } else if case.branches == 0 {
        latest_matches
    } else {
        data.sequence_ok
            && done == count
            && data.next_event == count
            && data.next_branch == 0
            && data.offered.is_empty()
            && data.probes.iter().all(|p| p.count as usize == count)
    };
    let physically_idle = snapshot.execution.idle
        && snapshot.execution.executing.is_empty()
        && snapshot.execution.streaming.is_empty();
    let expected_errors = errors.iter().all(|e| cancel_run && e["code"] == "RUN003");
    let success = physically_idle
        && !timed_out
        && count > 0
        && count < 20_000_000
        && !accepted.nodes.is_empty()
        && expected_errors
        && validation_ok
        && !snapshot.recording_blocked
        && log.unconfirmed == 0
        && log.capture_failures == 0
        && values.as_ref().is_none_or(|v| v.failures == 0)
        && data.sends.count as usize == count
        && m.subscriptions.load(Ordering::SeqCst) == 1;
    let settled = data
        .start
        .map(|start| (wall + elapsed).duration_since(start).as_secs_f64());
    let result = serde_json::json!({
        "schema":1,"case":case.name,"mode":mode,"requested_seconds":seconds,
        "payload_bytes":bytes,"requested_delay_us":delay_us,"cancel_run":cancel_run,"cancel_ms":cancel_ms,
        "success":success,"physically_idle":physically_idle,"timed_out":timed_out,"sent":count,"probe_completed":done,
        "validation":"online full sequence and values for probes; final value only for pure controls; prefix only on cancellation",
        "sequence_ok":(case.branches>0).then_some(data.sequence_ok),"latest_matches":latest_matches,
        "validation_ok":validation_ok,"branches":case.branches,"source_subscriptions":m.subscriptions.load(Ordering::SeqCst),
        "settled_ms":settled.map(|s|s*1000.0),"settled_events_per_second":settled.map(|s|count as f64/s),
        "source_ms":data.start.zip(data.source_end).map(|(a,b)|b.duration_since(a).as_secs_f64()*1000.0),
        "tail_ms":data.source_end.map(|end|(wall+elapsed).duration_since(end).as_secs_f64()*1000.0),
        "send":data.sends.json(),"probe_entry":data.probes.iter().map(Histogram::json).collect::<Vec<_>>(),
        "max_offered_unfinished":(case.branches>0).then_some(data.max_offered_unfinished),
        "cancellation_timestamps_omitted":data.cancellation_timestamps_omitted,
        "heartbeats":heartbeats,"recording_blocked":snapshot.recording_blocked,
        "log_entries":log.entries.len(),"log_omitted":log.omitted,"log_unconfirmed":log.unconfirmed,
        "log_capture_failures":log.capture_failures,"storage_failures":values.map(|v|v.failures),
        "graph_nodes":graph_nodes,"files_bytes_before_shutdown":state_bytes,
        "files_bytes_after_shutdown":bytes_under(root.path()),"errors":errors,
        "diagnostics":format!("{:?}",accepted.diagnostics),
    });
    println!("{result}");
    if !success {
        std::process::exit(1);
    }
}
