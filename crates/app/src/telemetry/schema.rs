use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Closed dimensions: user strings cannot create labels, paths or log payloads.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Startup,
    Execution,
    Queue,
    Http,
    Journal,
    Web,
    Projection,
    UiSubmit,
}
impl Operation {
    pub const ALL: [Self; 8] = [
        Self::Startup,
        Self::Execution,
        Self::Queue,
        Self::Http,
        Self::Journal,
        Self::Web,
        Self::Projection,
        Self::UiSubmit,
    ];
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "startup" => Self::Startup,
            "execution" => Self::Execution,
            "queue" => Self::Queue,
            "http" => Self::Http,
            "journal" => Self::Journal,
            "web" => Self::Web,
            "projection" => Self::Projection,
            "ui_submit" => Self::UiSubmit,
            _ => return None,
        })
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Error,
    Cancelled,
    Timeout,
    Rejected,
    Abandoned,
}
impl Outcome {
    pub const ALL: [Self; 6] = [
        Self::Ok,
        Self::Error,
        Self::Cancelled,
        Self::Timeout,
        Self::Rejected,
        Self::Abandoned,
    ];
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ok" => Self::Ok,
            "error" => Self::Error,
            "cancelled" => Self::Cancelled,
            "timeout" => Self::Timeout,
            "rejected" => Self::Rejected,
            "abandoned" => Self::Abandoned,
            _ => return None,
        })
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Notice {
    Started,
    Stopped,
    Panic,
    UiError,
    UiRejection,
    UiRender,
    Reconnect,
}
impl Notice {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "started" => Self::Started,
            "stopped" => Self::Stopped,
            "panic" => Self::Panic,
            "ui_error" => Self::UiError,
            "ui_rejection" => Self::UiRejection,
            "ui_render" => Self::UiRender,
            "reconnect" => Self::Reconnect,
            _ => return None,
        })
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum Record {
    Operation {
        at_ms: u64,
        id: u64,
        parent: Option<u64>,
        kind: Operation,
        outcome: Outcome,
        elapsed_us: u64,
    },
    Notice {
        at_ms: u64,
        kind: Notice,
    },
}
impl Record {
    pub fn important(&self) -> bool {
        match self {
            Self::Notice { .. } => true,
            Self::Operation { outcome, .. } => matches!(
                outcome,
                Outcome::Error | Outcome::Timeout | Outcome::Rejected | Outcome::Abandoned
            ),
        }
    }
}
/// Noncumulative buckets; last bucket includes all larger durations.
pub const BOUNDS_US: [u64; 8] = [
    100,
    1000,
    10_000,
    100_000,
    1_000_000,
    10_000_000,
    60_000_000,
    u64::MAX,
];
pub struct Metrics {
    active: [AtomicU64; 8],
    counts: [[AtomicU64; 6]; 8],
    buckets: [[AtomicU64; 8]; 8],
    sums: [AtomicU64; 8],
    notices: [AtomicU64; 7],
}
impl Default for Metrics {
    fn default() -> Self {
        Self {
            active: std::array::from_fn(|_| AtomicU64::new(0)),
            counts: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
            buckets: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU64::new(0))),
            sums: std::array::from_fn(|_| AtomicU64::new(0)),
            notices: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}
#[derive(Serialize)]
pub struct MetricSnapshot {
    pub in_flight: u64,
    pub operation: Operation,
    pub outcomes: Vec<(Outcome, u64)>,
    pub sum_us: u64,
    pub buckets: Vec<u64>,
}
#[derive(Serialize)]
pub struct Snapshot {
    pub operations: Vec<MetricSnapshot>,
    pub notices: Vec<u64>,
    pub bucket_upper_bounds_us: [u64; 8],
}
impl Metrics {
    pub fn begin(&self, operation: Operation) {
        self.active[operation as usize].fetch_add(1, Relaxed);
    }
    pub fn end(&self, operation: Operation) {
        self.active[operation as usize].fetch_sub(1, Relaxed);
    }
    pub fn record(&self, record: Record) {
        match record {
            Record::Operation {
                kind,
                outcome,
                elapsed_us,
                ..
            } => {
                self.counts[kind as usize][outcome as usize].fetch_add(1, Relaxed);
                self.sums[kind as usize].fetch_add(elapsed_us, Relaxed);
                let bucket = BOUNDS_US.iter().position(|b| elapsed_us <= *b).unwrap_or(7);
                self.buckets[kind as usize][bucket].fetch_add(1, Relaxed);
            }
            Record::Notice { kind, .. } => {
                self.notices[kind as usize].fetch_add(1, Relaxed);
            }
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            operations: Operation::ALL
                .iter()
                .map(|op| MetricSnapshot {
                    operation: *op,
                    in_flight: self.active[*op as usize].load(Relaxed),
                    outcomes: Outcome::ALL
                        .iter()
                        .map(|o| (*o, self.counts[*op as usize][*o as usize].load(Relaxed)))
                        .collect(),
                    sum_us: self.sums[*op as usize].load(Relaxed),
                    buckets: self.buckets[*op as usize]
                        .iter()
                        .map(|v| v.load(Relaxed))
                        .collect(),
                })
                .collect(),
            notices: self.notices.iter().map(|v| v.load(Relaxed)).collect(),
            bucket_upper_bounds_us: BOUNDS_US,
        }
    }
    pub fn clear(&self) {
        for v in self
            .counts
            .iter()
            .flatten()
            .chain(self.buckets.iter().flatten())
            .chain(self.sums.iter())
            .chain(self.notices.iter())
            .chain(self.active.iter())
        {
            v.store(0, Relaxed);
        }
    }
}
