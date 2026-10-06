//! Optional payload-free MCP wire accounting. No model/tokenizer knowledge.
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Write},
    path::Path,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy)]
pub(super) enum Outcome {
    Ok,
    Failed,
    Rejected,
    Unavailable,
    ProtocolError,
    Notification,
    TransportError,
    InputLimit,
}
impl Outcome {
    fn key(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "tool_failed",
            Self::Rejected => "rejected",
            Self::Unavailable => "unavailable",
            Self::ProtocolError => "protocol_error",
            Self::Notification => "no_reply",
            Self::TransportError => "transport_error",
            Self::InputLimit => "input_limit",
        }
    }
}

#[derive(Clone, Default, Serialize)]
struct Counters {
    messages: u64,
    completed: u64,
    input_bytes: u64,
    output_bytes: u64,
    duration_us: u64,
    max_duration_us: u64,
    outcomes: BTreeMap<&'static str, u64>,
}
impl Counters {
    fn receive(&mut self, bytes: usize) {
        self.messages = self.messages.saturating_add(1);
        self.input_bytes = self.input_bytes.saturating_add(bytes as u64);
    }
    fn finish(&mut self, bytes: u64, duration: u64, outcome: Outcome) {
        self.completed = self.completed.saturating_add(1);
        self.output_bytes = self.output_bytes.saturating_add(bytes);
        self.duration_us = self.duration_us.saturating_add(duration);
        self.max_duration_us = self.max_duration_us.max(duration);
        let count = self.outcomes.entry(outcome.key()).or_default();
        *count = count.saturating_add(1);
    }
}

#[derive(Clone, Serialize)]
struct Report {
    schema: &'static str,
    session: String,
    started_unix_ms: u128,
    elapsed_ms: u128,
    state: &'static str,
    totals: Counters,
    groups: BTreeMap<&'static str, Counters>,
    buckets: BTreeMap<String, Counters>,
}
#[derive(Clone, Default)]
pub(super) struct Meter(Option<Arc<Mutex<Report>>>);

/// A ticket follows its reply through concurrent dispatch and the single output writer.
#[derive(Clone)]
pub(super) struct Ticket {
    meter: Meter,
    group: &'static str,
    bucket: String,
    started: Instant,
}
impl Meter {
    fn new() -> Self {
        Self(Some(Arc::new(Mutex::new(Report {
            schema: "wes.mcp-metrics.v1",
            session: uuid::Uuid::new_v4().to_string(),
            started_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            elapsed_ms: 0,
            state: "live",
            totals: Counters::default(),
            groups: BTreeMap::new(),
            buckets: BTreeMap::new(),
        }))))
    }
    #[cfg(test)]
    pub(super) fn testing() -> Self {
        Self::new()
    }
    #[cfg(test)]
    pub(super) fn snapshot(&self) -> Value {
        serde_json::to_value(&*self.0.as_ref().unwrap().lock().unwrap()).unwrap()
    }
    pub(super) fn receive(
        &self,
        message: Option<&Value>,
        bytes: usize,
        known_tools: &BTreeSet<String>,
    ) -> Ticket {
        if self.0.is_none() {
            return Ticket {
                meter: self.clone(),
                group: "",
                bucket: String::new(),
                started: Instant::now(),
            };
        }
        let (group, name) = match message.and_then(|v| v["method"].as_str()) {
            Some("initialize") => ("discovery", "initialize"),
            Some("tools/list") => ("discovery", "tools/list"),
            Some("tools/call") => {
                let name = message.and_then(|v| v["params"]["name"].as_str());
                (
                    "tools",
                    name.filter(|n| known_tools.contains(*n)).unwrap_or("other"),
                )
            }
            Some("ping") => ("control", "ping"),
            Some("notifications/initialized") => ("control", "notifications/initialized"),
            Some(_) => ("control", "other"),
            None => ("control", "invalid"),
        };
        let ticket = Ticket {
            meter: self.clone(),
            group,
            bucket: format!("{group}/{name}"),
            started: Instant::now(),
        };
        if let Some(report) = &self.0 {
            let mut report = report.lock().unwrap_or_else(|e| e.into_inner());
            report.totals.receive(bytes);
            report.groups.entry(group).or_default().receive(bytes);
            report
                .buckets
                .entry(ticket.bucket.clone())
                .or_default()
                .receive(bytes);
        }
        ticket
    }
}
impl Ticket {
    pub(super) fn finish(self, bytes: u64, outcome: Outcome) {
        if let Some(report) = &self.meter.0 {
            let elapsed = self.started.elapsed().as_micros().min(u64::MAX as u128) as u64;
            let mut report = report.lock().unwrap_or_else(|e| e.into_inner());
            report.totals.finish(bytes, elapsed, outcome);
            report
                .groups
                .entry(self.group)
                .or_default()
                .finish(bytes, elapsed, outcome);
            report
                .buckets
                .entry(self.bucket)
                .or_default()
                .finish(bytes, elapsed, outcome);
        }
    }
}

pub(super) struct Collection {
    pub meter: Meter,
    stop: Option<mpsc::Sender<u8>>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Collection {
    pub(super) fn from_env() -> Self {
        match std::env::var_os("WES_MCP_METRICS_DIR").filter(|s| !s.is_empty()) {
            Some(directory) => Self::start(Path::new(&directory), Duration::from_secs(1))
                .unwrap_or_else(|_| {
                    eprintln!("wes MCP metrics unavailable; tools remain enabled.");
                    Self::off()
                }),
            None => Self::off(),
        }
    }
    fn off() -> Self {
        Self {
            meter: Meter::default(),
            stop: None,
            worker: None,
        }
    }
    fn start(directory: &Path, interval: Duration) -> io::Result<Self> {
        std::fs::create_dir_all(directory)?;
        let meter = Meter::new();
        let path = directory.join(format!(
            "mcp-{}.json",
            meter.0.as_ref().unwrap().lock().unwrap().session
        ));
        let began = Instant::now();
        publish(&meter, &path, began, "live")?;
        let (stop, stopped) = mpsc::channel();
        let recording = meter.clone();
        let worker = std::thread::Builder::new()
            .name("wes-mcp-metrics".into())
            .spawn(move || {
                let mut previous = (0, 0);
                loop {
                    let state = match stopped.recv_timeout(interval) {
                        Err(mpsc::RecvTimeoutError::Timeout) => "live",
                        Ok(0) => "closed",
                        Ok(2) => "input_limit",
                        Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => "transport_error",
                    };
                    let activity = {
                        let report = recording
                            .0
                            .as_ref()
                            .unwrap()
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        (report.totals.messages, report.totals.completed)
                    };
                    if state == "live" && activity == previous {
                        continue;
                    }
                    match publish(&recording, &path, began, state) {
                        Ok(published) => previous = published,
                        Err(error) => {
                            // Host classification is useful without disclosing paths or payloads.
                            eprintln!("wes MCP metrics export stopped: {:?}, OS error {:?}; tools remain enabled.", error.kind(), error.raw_os_error());
                            break;
                        }
                    }
                    if state != "live" {
                        break;
                    }
                }
            })?;
        Ok(Self {
            meter,
            stop: Some(stop),
            worker: Some(worker),
        })
    }
    pub(super) fn finish(mut self, code: u8) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(code);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn publish(
    meter: &Meter,
    path: &Path,
    began: Instant,
    state: &'static str,
) -> io::Result<(u64, u64)> {
    let mut report = meter
        .0
        .as_ref()
        .expect("enabled")
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    report.elapsed_ms = began.elapsed().as_millis();
    report.state = state;
    // Private tempfile plus atomic replacement: readers see one complete snapshot.
    crate::file_snapshot::write(path, |file| {
        serde_json::to_writer_pretty(&mut *file, &report)?;
        file.write_all(b"\n")
    })?;
    Ok((report.totals.messages, report.totals.completed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replacing_a_snapshot_preserves_an_open_readers_complete_old_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("snapshot.json");
        let meter = Meter::new();
        let began = Instant::now();
        publish(&meter, &path, began, "live").unwrap();
        let reader = std::fs::File::open(&path).unwrap();
        meter
            .receive(Some(&json!({"method":"ping"})), 20, &BTreeSet::new())
            .finish(30, Outcome::Ok);
        publish(&meter, &path, began, "live").unwrap();
        let old: Value = serde_json::from_reader(reader).unwrap();
        let new: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(old["totals"]["completed"], 0);
        assert_eq!(new["totals"]["completed"], 1);
    }

    #[test]
    fn live_and_final_exports_are_atomic_private_and_session_scoped() {
        let directory = tempfile::tempdir().unwrap();
        let first = Collection::start(directory.path(), Duration::from_millis(10)).unwrap();
        let second = Collection::start(directory.path(), Duration::from_millis(10)).unwrap();
        let registry = BTreeSet::from(["execute".to_owned()]);
        let ticket = first.meter.receive(Some(&json!({"method":"tools/call","params":{"name":"execute","arguments":{"source":"secret"}}})), 101, &registry);
        ticket.finish(202, Outcome::Ok);
        let path = directory.path().join(format!(
            "mcp-{}.json",
            first.meter.snapshot()["session"].as_str().unwrap()
        ));
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let live: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(live["state"], "live");
            if live["totals"]["completed"] == 1 {
                break;
            }
            assert!(Instant::now() < deadline, "live export did not update");
            std::thread::sleep(Duration::from_millis(2));
        }
        let settled = std::fs::read(&path).unwrap();
        std::thread::sleep(Duration::from_millis(35));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            settled,
            "idle connections must not rewrite reports"
        );
        first.finish(0);
        second.finish(2);
        let files: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|p| p.unwrap().path())
            .collect();
        assert_eq!(files.len(), 2);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("secret"));
        let report: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(report["state"], "closed");
        assert_eq!(report["totals"]["input_bytes"], 101);
        assert_eq!(report["totals"]["output_bytes"], 202);
        assert_eq!(report["groups"]["tools"]["messages"], 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let invalid = directory.path().join("not-a-directory");
        std::fs::write(&invalid, "fixture").unwrap();
        assert!(Collection::start(&invalid, Duration::from_secs(1)).is_err());
    }

    #[test]
    fn unknown_labels_are_bounded_and_never_record_client_text() {
        let meter = Meter::testing();
        let registry = BTreeSet::new();
        for i in 0..1000 {
            let message = json!({"method":"tools/call","params":{"name":format!("secret-{i}")}});
            meter
                .receive(Some(&message), 7, &registry)
                .finish(0, Outcome::Rejected);
        }
        let report = meter.snapshot();
        assert_eq!(report["buckets"].as_object().unwrap().len(), 1);
        assert_eq!(report["buckets"]["tools/other"]["messages"], 1000);
        assert!(!report.to_string().contains("secret"));
    }
}

/// Count bytes actually accepted by the stdio writer, including framing/partial writes.
pub(super) struct CountingWriter<'a, W> {
    pub writer: &'a mut W,
    pub bytes: u64,
}
impl<W: Write> Write for CountingWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let n = self.writer.write(bytes)?;
        self.bytes = self.bytes.saturating_add(n as u64);
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}
