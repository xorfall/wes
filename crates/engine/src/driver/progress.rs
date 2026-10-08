//! Bounded lossy status notifications. They carry no payload, source identity or
//! authority and never enter the graph/history as values or successful outputs.
use crate::runtime::Run;
use tokio::sync::mpsc;
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Reading,
    Processing,
    Finishing,
    Complete,
    Stopped,
    Cancelled,
}
impl Phase {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Reading => "reading",
            Self::Processing => "processing",
            Self::Finishing => "finishing",
            Self::Complete => "complete",
            Self::Stopped => "stopped",
            Self::Cancelled => "cancelled",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionUnit {
    Bytes,
    Records,
}
impl PositionUnit {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Bytes => "bytes",
            Self::Records => "records",
        }
    }
}
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counters {
    pub committed_position: u64,
    pub read_position: u64,
    pub extent: u64,
    pub unit: PositionUnit,
    pub input_records: u64,
    pub output_records: u64,
    pub work: u64,
    pub work_allowance: u64,
    pub work_limit: u64,
    pub held_charge: u64,
    pub high_water_charge: u64,
    pub held_limit: u64,
    pub output_charge: u64,
    pub output_limit: u64,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct ExecutionProgress {
    kind: &'static str,
    pub phase: Phase,
    pub counters: Option<Counters>,
}
impl ExecutionProgress {
    pub fn records(phase: Phase, counters: Option<Counters>) -> Self {
        Self {
            kind: "records",
            phase,
            counters,
        }
    }
    pub fn description(&self) -> wes_core::Data {
        use wes_core::Data;
        let counters = self.counters.as_ref().map(|c| {
            Data::Record(
                [
                    (
                        "committedPosition".into(),
                        Data::Int(c.committed_position as i64),
                    ),
                    ("readPosition".into(), Data::Int(c.read_position as i64)),
                    ("extent".into(), Data::Int(c.extent as i64)),
                    ("unit".into(), Data::Text(c.unit.name().into())),
                    ("inputRecords".into(), Data::Int(c.input_records as i64)),
                    ("outputRecords".into(), Data::Int(c.output_records as i64)),
                    ("work".into(), Data::Int(c.work as i64)),
                    ("workAllowance".into(), Data::Int(c.work_allowance as i64)),
                    ("workLimit".into(), Data::Int(c.work_limit as i64)),
                    ("heldCharge".into(), Data::Int(c.held_charge as i64)),
                    (
                        "highWaterCharge".into(),
                        Data::Int(c.high_water_charge as i64),
                    ),
                    ("heldLimit".into(), Data::Int(c.held_limit as i64)),
                    ("outputCharge".into(), Data::Int(c.output_charge as i64)),
                    ("outputLimit".into(), Data::Int(c.output_limit as i64)),
                ]
                .into(),
            )
        });
        Data::Record(
            [
                ("kind".into(), Data::Text(self.kind.into())),
                ("phase".into(), Data::Text(self.phase.name().into())),
                ("counters".into(), Data::Option(counters.map(Box::new))),
            ]
            .into(),
        )
    }
    pub fn restricted(mut self, policy: &wes_core::flow::FlowPolicy) -> Self {
        if policy.is_private() || policy.is_unknown() {
            self.counters = None;
        }
        self
    }
}
pub(crate) struct Update {
    pub run: Run,
    pub progress: ExecutionProgress,
}
#[derive(Clone, Default)]
pub struct Reporter {
    target: Option<(Run, mpsc::Sender<Update>)>,
    policy: wes_core::flow::FlowPolicy,
}
impl Reporter {
    pub(crate) fn new(run: Run, sender: mpsc::Sender<Update>) -> Self {
        Self {
            target: Some((run, sender)),
            policy: Default::default(),
        }
    }
    pub fn silent() -> Self {
        Self::default()
    }
    pub fn with_policy(mut self, policy: &wes_core::flow::FlowPolicy) -> Self {
        self.policy = self.policy.join(policy);
        self
    }
    /// Queue capacity and producer cadence bound notifications. Lossy progress
    /// never blocks processing; the terminal receipt contains committed totals.
    pub fn report(&self, progress: ExecutionProgress) {
        if let Some((run, sender)) = &self.target {
            let _ = sender.try_send(Update {
                run: run.clone(),
                progress: progress.restricted(&self.policy),
            });
        }
    }
}
