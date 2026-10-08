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
    Committing,
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
            Self::Committing => "committing",
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
#[serde(rename_all = "camelCase")]
pub struct RecordingCounters {
    pub state: &'static str,
    pub first: String,
    pub accepted_through: String,
    pub committed_through: String,
    pub pending: Option<String>,
    pub rejected: String,
    pub termination: Option<crate::storage::datasets::RecordingEnd>,
    pub charged_bytes: String,
    pub charged_work: String,
    pub bytes_limit: String,
    pub work_limit: String,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct ExecutionProgress {
    #[serde(skip)]
    policy: wes_core::flow::FlowPolicy,
    kind: &'static str,
    pub phase: Phase,
    pub counters: Option<Counters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recording: Option<RecordingCounters>,
}
impl ExecutionProgress {
    pub fn records(phase: Phase, counters: Option<Counters>) -> Self {
        Self {
            policy: Default::default(),
            kind: "records",
            phase,
            counters,
            recording: None,
        }
    }
    pub fn recording(status: &crate::eventlog::Status, limits: crate::eventlog::Limits) -> Self {
        use crate::eventlog::Phase as Writer;
        let (phase, state) = match status.phase {
            Writer::Prepared => (Phase::Reading, "prepared"),
            Writer::Recording => (Phase::Processing, "recording"),
            Writer::Draining => (Phase::Committing, "draining"),
            Writer::Stopped => (Phase::Complete, "stopped"),
            Writer::Incomplete => (Phase::Stopped, "incomplete"),
            Writer::Unconfirmed => (Phase::Stopped, "unconfirmed"),
        };
        Self {
            policy: Default::default(),
            kind: "recording",
            phase,
            counters: None,
            recording: Some(RecordingCounters {
                state,
                first: status.coverage.first.to_string(),
                accepted_through: status.coverage.accepted_through.to_string(),
                committed_through: status.coverage.committed_through.to_string(),
                pending: status.coverage.pending.map(|n| n.to_string()),
                rejected: status.coverage.rejected.to_string(),
                termination: status.coverage.termination,
                charged_bytes: status.charged_bytes.to_string(),
                charged_work: status.charged_work.to_string(),
                bytes_limit: limits.bytes.to_string(),
                work_limit: limits.work.to_string(),
            }),
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
        let mut fields = [
            ("kind".into(), Data::Text(self.kind.into())),
            ("phase".into(), Data::Text(self.phase.name().into())),
            ("counters".into(), Data::Option(counters.map(Box::new))),
        ]
        .into_iter()
        .collect::<indexmap::IndexMap<String, Data>>();
        if self.kind == "recording" {
            fields.insert(
                "recording".into(),
                Data::Option(self.recording.as_ref().map(|r| {
                    Box::new(Data::Record(
                        [
                            ("state".into(), Data::Text(r.state.into())),
                            ("first".into(), Data::Text(r.first.as_str().into())),
                            (
                                "acceptedThrough".into(),
                                Data::Text(r.accepted_through.as_str().into()),
                            ),
                            (
                                "committedThrough".into(),
                                Data::Text(r.committed_through.as_str().into()),
                            ),
                            (
                                "pending".into(),
                                Data::Option(
                                    r.pending
                                        .as_ref()
                                        .map(|s| Box::new(Data::Text(s.as_str().into()))),
                                ),
                            ),
                            ("rejected".into(), Data::Text(r.rejected.as_str().into())),
                            (
                                "termination".into(),
                                Data::Option(r.termination.map(|end| {
                                    Box::new(Data::Text(
                                        serde_json::to_value(end)
                                            .expect("recording enum")
                                            .as_str()
                                            .expect("enum string")
                                            .into(),
                                    ))
                                })),
                            ),
                            (
                                "chargedBytes".into(),
                                Data::Text(r.charged_bytes.as_str().into()),
                            ),
                            (
                                "chargedWork".into(),
                                Data::Text(r.charged_work.as_str().into()),
                            ),
                            (
                                "bytesLimit".into(),
                                Data::Text(r.bytes_limit.as_str().into()),
                            ),
                            ("workLimit".into(), Data::Text(r.work_limit.as_str().into())),
                        ]
                        .into(),
                    ))
                })),
            );
        }
        Data::Record(fields)
    }
    pub(crate) fn blocked_by(&self, access: &crate::storage::datasets::DatasetAccess) -> bool {
        self.policy
            .dataset_reads()
            .iter()
            .any(|origin| access.closed || access.withdrawn.contains(origin.dataset()))
    }
    pub fn restricted(mut self, policy: &wes_core::flow::FlowPolicy) -> Self {
        self.policy = self.policy.join(policy);
        if policy.is_confidential() || policy.is_unknown() {
            self.counters = None;
            self.recording = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_status_keeps_exact_sequences_and_withdraws_both_wire_and_query_counters() {
        use crate::storage::datasets::*;
        let schema = wes_core::contracts::ResolvedContractBundle::capture(
            wes_core::contracts::ContractRegistry::new()
                .resolve("Int")
                .unwrap(),
            Default::default(),
        )
        .unwrap();
        let reference = wes_core::DatasetRef::new(
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            1,
            uuid::Uuid::new_v4().to_string(),
            format!("sha256:{}", "0".repeat(64)),
            1,
            schema.digest().into(),
            0,
            1,
        );
        // The wire does not coerce offsets into JavaScript numbers, even for an empty prefix.
        let reference = reference.expect("synthetic descriptor");
        let status = crate::eventlog::Status {
            phase: crate::eventlog::Phase::Unconfirmed,
            reference,
            coverage: EventLogCoverage {
                run: uuid::Uuid::new_v4().to_string(),
                epoch: uuid::Uuid::new_v4().to_string(),
                first: u64::MAX - 2,
                accepted_through: u64::MAX,
                committed_through: u64::MAX - 1,
                pending: None,
                rejected: 1,
                termination: Some(RecordingEnd::Unconfirmed),
            },
            charged_bytes: 2,
            charged_work: 3,
        };
        let progress = ExecutionProgress::recording(&status, Default::default());
        let wire = serde_json::to_value(&progress).unwrap();
        assert_eq!(wire["kind"], "recording");
        assert_eq!(wire["recording"]["acceptedThrough"], u64::MAX.to_string());
        assert!(wire["recording"]["pending"].is_null());
        let hidden = progress.restricted(&wes_core::flow::FlowPolicy::default().private());
        let wire = serde_json::to_value(&hidden).unwrap();
        assert!(wire.get("recording").is_none());
        assert!(wire["counters"].is_null());
        let wes_core::Data::Record(fields) = hidden.description() else {
            panic!();
        };
        assert_eq!(fields["recording"], wes_core::Data::Option(None));
        let ordinary =
            serde_json::to_value(ExecutionProgress::records(Phase::Reading, None)).unwrap();
        assert_eq!(
            ordinary,
            serde_json::json!({"kind":"records","phase":"reading","counters":null})
        );
    }
}
