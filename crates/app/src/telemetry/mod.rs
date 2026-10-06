//! Local, optional application diagnostics. Never stores command/value/error text.
mod files;
mod layer;
pub mod schema;
#[cfg(test)]
mod tests;

use schema::{Metrics, Notice, Record};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{
            AtomicBool, AtomicU8, AtomicU64,
            Ordering::{Acquire, Relaxed, Release},
        },
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tracing_subscriber::prelude::*;

const RECENT: usize = 512;
const QUEUE: usize = 1024;
pub const CAPTURE_SECONDS: u64 = 300;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Off = 0,
    Basic = 1,
    Diagnostic = 2,
}
impl Mode {
    fn from(value: u8) -> Self {
        match value {
            1 => Self::Basic,
            2 => Self::Diagnostic,
            _ => Self::Off,
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Preference {
    schema: u8,
    mode: Mode,
}
#[derive(Clone, Copy)]
struct Message {
    epoch: u64,
    capture: u64,
    record: Record,
}
struct Worker {
    sender: SyncSender<Option<Message>>,
    stopped: Arc<AtomicBool>,
    done: mpsc::Receiver<()>,
    thread: Option<thread::JoinHandle<()>>,
}
struct State {
    worker: Option<Worker>,
    recent: VecDeque<Record>,
    last_second: u64,
    emitted: usize,
    client_second: u64,
    client_batches: usize,
}
pub struct Controller {
    root: PathBuf,
    _lock: wes_adapters::file_lock::ExclusiveLock,
    origin: Instant,
    started_at_ms: u64,
    mode: AtomicU8,
    saved: AtomicU8,
    forced: bool,
    until: AtomicU64,
    capture: AtomicU64,
    pub(crate) epoch: AtomicU64,
    pub(crate) sequence: AtomicU64,
    gate: RwLock<()>,
    state: Mutex<State>,
    files: Mutex<files::Files>,
    pub control: Mutex<()>,
    metrics: Metrics,
    dropped: AtomicU64,
    suppressed: AtomicU64,
    write_errors: AtomicU64,
    previous_unclean: bool,
}
static GLOBAL: OnceLock<Arc<Controller>> = OnceLock::new();
pub fn global() -> Option<Arc<Controller>> {
    GLOBAL.get().cloned()
}

impl Controller {
    /// Explicit construction only. Embedded runtimes do not call this automatically.
    pub fn open(root: PathBuf, forced: Option<Mode>) -> io::Result<Arc<Self>> {
        if forced == Some(Mode::Diagnostic) {
            return Err(io::Error::other(
                "Diagnostic mode cannot be persisted or forced.",
            ));
        }
        crate::data_home::private_directory(&root)?;
        let lock =
            wes_adapters::api_library::exclusive_lock(&root, ".telemetry.lock").map_err(|_| {
                io::Error::other("Telemetry directory is already in use or unavailable.")
            })?;
        let preference = match crate::data_home::metadata::<Preference>(&root.join("settings.json"))
        {
            Ok(p) if p.schema == 1 && p.mode != Mode::Diagnostic => p.mode,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Mode::Basic,
            _ => {
                return Err(io::Error::other(
                    "Telemetry settings are unreadable; collection remains disabled.",
                ));
            }
        };
        let mode = forced.unwrap_or(preference);
        let mut files = files::Files::new(root.clone())?;
        if mode != Mode::Off {
            files.remove_expired_capture()?;
        }
        let previous_unclean = root.join("running.json").try_exists()?;
        let this = Arc::new(Self {
            root,
            _lock: lock,
            origin: Instant::now(),
            started_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            mode: AtomicU8::new(mode as u8),
            saved: AtomicU8::new(mode as u8),
            forced: forced.is_some(),
            until: AtomicU64::new(0),
            capture: AtomicU64::new(0),
            epoch: AtomicU64::new(1),
            sequence: AtomicU64::new(1),
            gate: RwLock::new(()),
            state: Mutex::new(State {
                worker: None,
                recent: VecDeque::new(),
                last_second: 0,
                emitted: 0,
                client_second: 0,
                client_batches: 0,
            }),
            files: Mutex::new(files),
            control: Mutex::new(()),
            metrics: Metrics::default(),
            dropped: AtomicU64::new(0),
            suppressed: AtomicU64::new(0),
            write_errors: AtomicU64::new(0),
            previous_unclean,
        });
        if mode != Mode::Off {
            this.start_writer()?;
            this.marker(true)?;
            this.notice(Notice::Started);
        }
        Ok(this)
    }
    pub fn dispatch(self: &Arc<Self>) -> tracing::Dispatch {
        tracing::Dispatch::new(
            tracing_subscriber::registry().with(layer::TelemetryLayer(self.clone())),
        )
    }
    pub fn enabled(&self) -> bool {
        self.mode.load(Relaxed) != 0
    }
    pub(crate) fn elapsed_ms(&self) -> u64 {
        self.origin.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
    pub fn mode(&self) -> Mode {
        Mode::from(self.mode.load(Acquire))
    }
    fn marker(&self, running: bool) -> io::Result<()> {
        let path = self.root.join("running.json");
        if running {
            wes_adapters::api_library::atomic_json(&path, &serde_json::json!({"schema":1}))
                .map_err(io::Error::other)
        } else {
            match std::fs::symlink_metadata(&path) {
                Ok(m) if m.is_file() && !m.file_type().is_symlink() => std::fs::remove_file(path),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                _ => Err(io::Error::other("Invalid telemetry marker.")),
            }
        }
    }
    fn start_writer(self: &Arc<Self>) -> io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(worker) = &mut state.worker {
            if !worker.stopped.load(Acquire) {
                return Ok(());
            }
            if worker.thread.as_ref().is_some_and(|t| !t.is_finished()) {
                return Err(io::Error::other(
                    "The previous telemetry write is still finishing. Try again.",
                ));
            }
            if let Some(thread) = worker.thread.take() {
                let _ = thread.join();
            }
        }
        let (sender, receive) = mpsc::sync_channel::<Option<Message>>(QUEUE);
        let (done_tx, done) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let weak = Arc::downgrade(self);
        let thread = thread::Builder::new()
            .name("wes-telemetry".into())
            .spawn(move || {
                loop {
                    if stop.load(Acquire) {
                        break;
                    }
                    let Some(controller) = weak.upgrade() else {
                        break;
                    };
                    controller.expire();
                    // No sampling/polling worker exists in Off. Basic wakes once a minute at most.
                    let until = controller.until.load(Acquire);
                    let wait = if until == 0 {
                        Duration::from_secs(60)
                    } else {
                        Duration::from_millis(
                            until
                                .saturating_sub(controller.elapsed_ms())
                                .clamp(1, 60_000),
                        )
                    };
                    drop(controller);
                    match receive.recv_timeout(wait) {
                        Ok(Some(message)) => {
                            if stop.load(Acquire) {
                                break;
                            }
                            let Some(c) = weak.upgrade() else { break };
                            c.expire();
                            if !c.enabled() || message.epoch != c.epoch.load(Acquire) {
                                continue;
                            }
                            let capture = message.capture != 0;
                            if capture
                                && (c.mode() != Mode::Diagnostic
                                    || message.capture != c.capture.load(Acquire))
                            {
                                continue;
                            }
                            let result = {
                                let mut files = c.files.lock().unwrap_or_else(|e| e.into_inner());
                                if stop.load(Acquire)
                                    || !c.enabled()
                                    || message.epoch != c.epoch.load(Acquire)
                                    || (capture
                                        && (c.mode() != Mode::Diagnostic
                                            || message.capture != c.capture.load(Acquire)))
                                {
                                    continue;
                                }
                                files.write(message.record, capture)
                            };
                            match result {
                                Ok(false) => c.stop_capture_id(Some(message.capture)),
                                Err(_) => {
                                    c.write_errors.fetch_add(1, Relaxed);
                                    if capture {
                                        c.stop_capture_id(Some(message.capture));
                                    }
                                }
                                _ => {}
                            }
                        }
                        Ok(None) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                let _ = done_tx.send(());
            })?;
        state.worker = Some(Worker {
            sender,
            stopped,
            done,
            thread: Some(thread),
        });
        Ok(())
    }
    pub fn set_mode(self: &Arc<Self>, mode: Mode) -> io::Result<()> {
        if self.forced {
            return Err(io::Error::other(
                "Telemetry mode was fixed by WES_TELEMETRY for this process.",
            ));
        }
        if mode == Mode::Diagnostic {
            return Err(io::Error::other("Use a temporary capture instead."));
        }
        // Refusal to restart a still-stopping writer must not change the saved preference.
        if mode == Mode::Basic {
            self.start_writer()?;
        }
        if let Err(error) = wes_adapters::api_library::atomic_json(
            &self.root.join("settings.json"),
            &Preference { schema: 1, mode },
        ) {
            if !self.enabled() {
                self.clear_memory_and_stop();
            }
            return Err(io::Error::other(error));
        }
        self.saved.store(mode as u8, Release);
        {
            let _gate = self.gate.write().unwrap_or_else(|e| e.into_inner());
            self.until.store(0, Release);
            self.mode.store(mode as u8, Release);
            if mode == Mode::Off {
                self.clear_memory_and_stop();
            }
        }
        self.marker(mode != Mode::Off)?;
        Ok(())
    }
    fn clear_memory_and_stop(&self) {
        self.epoch.fetch_add(1, Release);
        self.metrics.clear();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.recent.clear();
        if let Some(w) = &state.worker {
            w.stopped.store(true, Release);
            let _ = w.sender.try_send(None);
        }
    }
    pub fn start_capture(self: &Arc<Self>) -> io::Result<()> {
        self.capture_for(Duration::from_secs(CAPTURE_SECONDS))
    }
    fn capture_for(self: &Arc<Self>, duration: Duration) -> io::Result<()> {
        if self.forced {
            return Err(io::Error::other(
                "Telemetry mode was fixed by WES_TELEMETRY for this process.",
            ));
        }
        if self.mode() == Mode::Diagnostic {
            return Err(io::Error::other("A diagnostic capture is already running."));
        }
        self.start_writer()?;
        if let Err(error) = self
            .files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .start_capture()
        {
            if !self.enabled() {
                self.clear_memory_and_stop();
            }
            return Err(error);
        }
        let _gate = self.gate.write().unwrap_or_else(|e| e.into_inner());
        self.capture.fetch_add(1, Release);
        self.until
            .store(self.elapsed_ms() + duration.as_millis() as u64, Release);
        self.mode.store(2, Release);
        if let Some(w) = &self.state.lock().unwrap_or_else(|e| e.into_inner()).worker {
            let _ = w.sender.try_send(None);
        }
        Ok(())
    }
    pub fn stop_capture(&self) {
        self.stop_capture_id(None);
    }
    fn stop_capture_id(&self, expected: Option<u64>) {
        let _gate = self.gate.write().unwrap_or_else(|e| e.into_inner());
        if self.mode() != Mode::Diagnostic
            || expected.is_some_and(|id| id != self.capture.load(Acquire))
        {
            return;
        }
        self.until.store(0, Release);
        self.mode.store(self.saved.load(Acquire), Release);
        if !self.enabled() {
            self.clear_memory_and_stop();
        }
    }
    fn expire(&self) {
        self.expire_at(self.elapsed_ms());
    }
    fn expire_at(&self, now_ms: u64) {
        let capture = self.capture.load(Acquire);
        let until = self.until.load(Acquire);
        if until > 0 && now_ms >= until {
            self.stop_capture_id(Some(capture));
        }
    }
    pub(crate) fn admit_client(&self) -> bool {
        let Ok(mut state) = self.state.try_lock() else {
            return false;
        };
        let second = self.elapsed_ms() / 1000;
        if state.client_second != second {
            state.client_second = second;
            state.client_batches = 0;
        }
        if state.client_batches >= 8 {
            return false;
        }
        state.client_batches += 1;
        true
    }
    fn panic_notice(&self) {
        // A panic can occur while ordinary logging holds a lock. Never wait/recurse here.
        let Ok(_gate) = self.gate.try_read() else {
            return;
        };
        if !self.enabled() {
            return;
        }
        let record = Record::Notice {
            at_ms: self.elapsed_ms(),
            kind: Notice::Panic,
        };
        self.metrics.record(record);
        if let Ok(state) = self.state.try_lock()
            && let Some(worker) = &state.worker
        {
            let _ = worker.sender.try_send(Some(Message {
                epoch: self.epoch.load(Acquire),
                capture: 0,
                record,
            }));
        }
    }
    pub fn notice(&self, kind: Notice) {
        self.record(
            self.epoch.load(Acquire),
            Record::Notice {
                at_ms: self.elapsed_ms(),
                kind,
            },
        );
    }
    pub(crate) fn begin(&self, operation: schema::Operation) -> Option<(u64, u64)> {
        let _gate = self.gate.read().unwrap_or_else(|e| e.into_inner());
        if !self.enabled() {
            return None;
        }
        self.metrics.begin(operation);
        Some((
            self.epoch.load(Acquire),
            self.sequence.fetch_add(1, Relaxed),
        ))
    }
    pub(crate) fn record(&self, epoch: u64, record: Record) {
        self.complete(epoch, record, false);
    }
    pub(crate) fn complete(&self, epoch: u64, record: Record, span: bool) {
        self.expire();
        if !self.enabled() {
            return;
        }
        let _gate = self.gate.read().unwrap_or_else(|e| e.into_inner());
        if !self.enabled() || self.epoch.load(Acquire) != epoch {
            return;
        }
        if span && let Record::Operation { kind, .. } = record {
            self.metrics.end(kind);
        }
        self.metrics.record(record);
        let Ok(mut state) = self.state.try_lock() else {
            self.dropped.fetch_add(1, Relaxed);
            return;
        };
        if state.recent.len() == RECENT {
            state.recent.pop_front();
        }
        state.recent.push_back(record);
        let capture = if self.mode() == Mode::Diagnostic {
            self.capture.load(Acquire)
        } else {
            0
        };
        if capture == 0 && !record.important() {
            return;
        }
        // A fixed global rate ceiling bounds duplicate error floods without a label map.
        let second = self.elapsed_ms() / 1000;
        if state.last_second != second {
            state.last_second = second;
            state.emitted = 0;
        }
        if state.emitted >= if capture == 0 { 20 } else { 500 } {
            self.suppressed.fetch_add(1, Relaxed);
            return;
        }
        state.emitted += 1;
        if state.worker.as_ref().is_none_or(|w| {
            w.sender
                .try_send(Some(Message {
                    epoch,
                    capture,
                    record,
                }))
                .is_err()
        }) {
            self.dropped.fetch_add(1, Relaxed);
        }
    }
    pub fn status(&self) -> serde_json::Value {
        self.expire();
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        serde_json::json!({"schema":1,"mode":self.mode(),"saved_mode":Mode::from(self.saved.load(Acquire)),"forced":self.forced,"remote_export":false,"remaining_ms":self.until.load(Acquire).saturating_sub(self.elapsed_ms()),
            "started_at_ms":self.started_at_ms,"uptime_ms":self.elapsed_ms(),"previous_unclean":self.previous_unclean,"dropped":self.dropped.load(Relaxed),"suppressed":self.suppressed.load(Relaxed),"write_errors":self.write_errors.load(Relaxed),
            "recent_count":state.recent.len(),"writer_stopping":state.worker.as_ref().is_some_and(|w| w.stopped.load(Acquire) && w.thread.as_ref().is_some_and(|t| !t.is_finished())),
            "metrics":self.metrics.snapshot(),"build":{"version":env!("CARGO_PKG_VERSION"),"revision":option_env!("WES_BUILD_ID").unwrap_or("unknown"),"debug":cfg!(debug_assertions),"os":std::env::consts::OS,"arch":std::env::consts::ARCH}})
    }
    pub fn export(&self) -> io::Result<serde_json::Value> {
        let status = self.status();
        let recent: Vec<_> = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recent
            .iter()
            .copied()
            .collect();
        let logs = self
            .files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .export()?;
        Ok(
            serde_json::json!({"schema":1,"status":status,"recent":recent,"logs":logs,"notice":"Local application diagnostics only. Export is a snapshot; records still queued may be absent."}),
        )
    }
    pub fn clear(&self) -> io::Result<()> {
        if self.enabled() {
            return Err(io::Error::other(
                "Turn telemetry off before clearing saved logs.",
            ));
        }
        if self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .worker
            .as_ref()
            .is_some_and(|w| w.thread.as_ref().is_some_and(|t| !t.is_finished()))
        {
            return Err(io::Error::other(
                "The telemetry writer is still stopping. Try again.",
            ));
        }
        self.files.lock().unwrap_or_else(|e| e.into_inner()).clear()
    }
    pub fn shutdown(&self) {
        self.notice(Notice::Stopped);
        let worker = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .worker
            .take();
        let mut finished = true;
        if let Some(w) = worker {
            let Worker {
                sender,
                stopped,
                done,
                mut thread,
            } = w;
            // Normal exit drains the queue. Off uses stopped=true and discards instead.
            drop(sender);
            finished = done.recv_timeout(Duration::from_secs(2)).is_ok();
            if finished {
                if let Some(t) = thread.take() {
                    let _ = t.join();
                }
            } else {
                stopped.store(true, Release);
            }
        }
        self.mode.store(0, Release);
        if finished {
            let _ = self.marker(false);
        }
    }
}
/// Own for the entire executable lifetime. Shutdown never replaces a business outcome.
pub struct Guard(Arc<Controller>);
impl Drop for Guard {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}
pub fn install(root: PathBuf) -> Option<Guard> {
    let forced = match std::env::var("WES_TELEMETRY") {
        Ok(s) if s == "off" => Some(Mode::Off),
        Ok(s) if s == "basic" => Some(Mode::Basic),
        Ok(_) => {
            eprintln!("wes: invalid WES_TELEMETRY; collection disabled");
            return None;
        }
        Err(_) => None,
    };
    let controller = match Controller::open(root, forced) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("wes: local diagnostics unavailable; collection disabled");
            return None;
        }
    };
    if tracing::dispatcher::set_global_default(controller.dispatch()).is_err() {
        controller.shutdown();
        return None;
    }
    let _ = GLOBAL.set(controller.clone());
    // Keep the original panic report on stderr, but never copy its potentially private text.
    let previous = std::panic::take_hook();
    let weak = Arc::downgrade(&controller);
    std::panic::set_hook(Box::new(move |info| {
        if let Some(c) = weak.upgrade() {
            c.panic_notice();
        }
        previous(info);
    }));
    Some(Guard(controller))
}
