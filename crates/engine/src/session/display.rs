//! Explicit, expiring demand for one latest completed public display sample per output.
use super::SessionError;
use crate::{
    graph::{NodeId, OutputPort, OutputRef},
    runtime::Observation,
    workspace::Workspace,
};
use std::collections::HashMap;
use tokio::time::{Duration, Instant};
const TTL: Duration = Duration::from_secs(3);
const PERIOD: Duration = Duration::from_millis(100);
fn max_views() -> usize {
    wes_budgets::get("display.views") as usize
}
// Retention is charged conservatively (including per-node overhead), independently
// of the transport encoder's 512 KiB byte / 50,000-node budget.
fn max_retained_charge() -> u64 {
    wes_budgets::get("display.bytes") as u64
}

#[derive(Clone, Debug)]
pub struct DisplaySource {
    pub node: NodeId,
    pub run: crate::runtime::RunId,
    pub phase: &'static str,
    pub counts: Option<(u64, u64, usize)>,
}
#[derive(Clone, Debug)]
pub struct DisplaySample {
    pub revision: u64,
    pub sources: Vec<DisplaySource>,
    pub over_budget: bool,
    pub producer_run: Option<crate::runtime::RunId>,
    pub value: Option<wes_core::Value>,
    pub epochs: Vec<(NodeId, crate::runtime::RunId)>,
}
struct Entry {
    expires: Instant,
    next: Instant,
    sample: DisplaySample,
}
/// A pending refresh is read-only unavailability, not withdrawal of the source.
struct Captured {
    sample: DisplaySample,
    waiting: bool,
}
#[derive(Default)]
pub(super) struct Displays {
    entries: HashMap<OutputRef, Entry>,
}
impl Displays {
    pub fn deadline(&self) -> Option<Instant> {
        self.entries.values().map(|entry| entry.expires).min()
    }
    pub fn expire(&mut self) {
        let now = Instant::now();
        self.entries.retain(|_, entry| entry.expires > now);
    }
    pub fn read(
        &mut self,
        workspace: &Workspace,
        node: &NodeId,
    ) -> Result<DisplaySample, SessionError> {
        self.read_output(workspace, &OutputRef::data(node.clone()))
    }
    pub fn read_output(
        &mut self,
        workspace: &Workspace,
        output: &OutputRef,
    ) -> Result<DisplaySample, SessionError> {
        self.expire();
        let captured = match capture(workspace, output) {
            Ok(sample) => sample,
            Err(error) => {
                self.entries.remove(output);
                return Err(error);
            }
        };
        let current = captured.sample;
        if !self.entries.contains_key(output) && self.entries.len() >= max_views() {
            return Err(SessionError::Capacity);
        }
        let now = Instant::now();
        let entry = self.entries.entry(output.clone()).or_insert_with(|| Entry {
            expires: now + TTL,
            next: now,
            sample: DisplaySample {
                revision: current.revision,
                sources: current.sources.clone(),
                over_budget: false,
                producer_run: None,
                value: None,
                epochs: current.epochs.clone(),
            },
        });
        entry.expires = now + TTL;
        if entry.sample.epochs != current.epochs {
            entry.sample = DisplaySample {
                revision: current.revision,
                sources: current.sources.clone(),
                over_budget: false,
                producer_run: None,
                value: None,
                epochs: current.epochs.clone(),
            };
            entry.next = now;
        }
        if captured.waiting {
            // Keep the bounded last sample internally, but do not report it as
            // current. View observation preserves its shown input and intent.
            return Ok(current);
        }
        Self::sample(entry, current, now);
        Ok(entry.sample.clone())
    }
    pub fn observe(&mut self, workspace: &Workspace, observation: &Observation) {
        self.expire();
        let now = Instant::now();
        for port in [OutputPort::Data, OutputPort::Error, OutputPort::Cancel] {
            let output = OutputRef {
                node: observation.node.clone(),
                port,
            };
            let Some(entry) = self.entries.get_mut(&output) else {
                continue;
            };
            // Authority always withdraws immediately, even between sampling ticks.
            if observation.error.as_ref().is_some_and(|error| {
                error.code() == "ENV020"
                    || error.policy().is_private()
                    || error.policy().is_unknown()
            }) || observation.value.as_ref().is_some_and(|value| {
                value.provenance().policy().is_private() || value.provenance().policy().is_unknown()
            }) {
                entry.sample.value = None;
                continue;
            }
            if now < entry.next {
                continue;
            }
            if let Ok(captured) = capture(workspace, &output) {
                if captured.waiting {
                    continue;
                }
                let mut sample = captured.sample;
                // A completion in this effect batch may already precede another entered attempt.
                // Select only this port's acknowledged completion, never substitute a data sample.
                let value = match (observation.state, port) {
                    (crate::graph::NodeState::Ready, OutputPort::Data) => observation.value.clone(),
                    (crate::graph::NodeState::Failed, OutputPort::Error) => {
                        observation.error.as_ref().map(|e| e.to_value())
                    }
                    (crate::graph::NodeState::Cancelled, OutputPort::Cancel) => observation
                        .error
                        .as_ref()
                        .map(|e| e.to_cancellation_value()),
                    _ => None,
                };
                if value.is_some() {
                    sample.value = value;
                    sample.producer_run = observation.run.clone();
                    sample.revision = observation.revision;
                }
                Self::sample(entry, sample, now);
            } else {
                entry.sample.value = None;
            }
        }
    }
    fn sample(entry: &mut Entry, mut sample: DisplaySample, now: Instant) {
        // Source status/counters can change without producing a value.
        if sample.epochs != entry.sample.epochs {
            entry.sample = sample.clone();
            entry.sample.value = None;
        }
        entry.sample.sources = sample.sources.clone();
        if entry.sample.value.is_none() && sample.value.is_none() {
            entry.sample.revision = sample.revision;
            entry.sample.producer_run = sample.producer_run.clone();
        }
        if now < entry.next
            || sample.value.is_none()
            || sample.revision == entry.sample.revision && entry.sample.value.is_some()
        {
            return;
        }
        entry.next = now + PERIOD;
        sample.over_budget = sample.value.as_ref().is_some_and(|value| {
            crate::value_size::value_charge(value, max_retained_charge()).is_none()
        });
        if sample.over_budget {
            sample.value = None;
        }
        entry.sample = sample;
    }
}
fn capture(workspace: &Workspace, output: &OutputRef) -> Result<Captured, SessionError> {
    use crate::{graph::NodeState, runtime::StaleReason};
    let node = &output.node;
    let runtime = workspace.runtime();
    if runtime.is_closed() {
        return Err(SessionError::Stopped);
    }
    let entry = runtime
        .graph()
        .node(node)
        .ok_or(SessionError::UnknownNode)?;
    if runtime.error_of(node).is_some_and(|error| {
        error.code() == "ENV020" || error.policy().is_private() || error.policy().is_unknown()
    }) {
        return Err(SessionError::Authority);
    }
    let value = runtime.value_of(node);
    if value.is_some_and(|value| {
        value.provenance().policy().is_private() || value.provenance().policy().is_unknown()
    }) {
        return Err(SessionError::Authority);
    }
    let mut pending = vec![node.clone()];
    let mut visited = std::collections::BTreeSet::new();
    let mut epochs = std::collections::BTreeMap::new();
    let mut waiting = false;
    while let Some(id) = pending.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let Some(ancestor) = runtime.graph().node(&id) else {
            return Err(SessionError::UnknownNode);
        };
        match runtime.stale_reason(&id) {
            Some(StaleReason::RefreshRequested | StaleReason::DependencyRefreshed) => {
                waiting = true;
            }
            None | Some(StaleReason::StreamUpdated | StaleReason::InputBehind) => {}
            Some(_) => return Err(SessionError::UnknownValue),
        }
        if runtime.value_of(&id).is_some_and(|value| {
            value.provenance().policy().is_private() || value.provenance().policy().is_unknown()
        }) {
            return Err(SessionError::Authority);
        }
        if runtime.error_of(&id).is_some_and(|error| {
            error.code() == "ENV020" || error.policy().is_private() || error.policy().is_unknown()
        }) {
            return Err(SessionError::Authority);
        }
        if ancestor
            .payload()
            .call()
            .is_some_and(crate::providers::BoundCall::streaming)
        {
            let run = runtime.run_of(&id).ok_or(SessionError::UnknownValue)?;
            epochs.insert(id, run.clone());
        } else {
            pending.extend(ancestor.dependencies().keys().cloned());
        }
    }
    let sources = epochs
        .iter()
        .map(|(id, run)| DisplaySource {
            node: id.clone(),
            run: run.clone(),
            phase: runtime.stream_phase(id),
            counts: runtime.stream_counts(id),
        })
        .collect();
    if epochs.is_empty() {
        if let Some(run) = runtime.run_of(node) {
            epochs.insert(node.clone(), run.clone());
        }
    }
    let value = if waiting {
        None
    } else {
        match output.port {
            OutputPort::Data => (matches!(entry.state(), NodeState::Ready | NodeState::Running))
                .then_some(value)
                .flatten()
                .cloned()
                .or_else(|| {
                    runtime
                        .evidence_value(node)
                        .map(|stopped| stopped.value.clone())
                }),
            OutputPort::Error | OutputPort::Cancel => match runtime.output(output) {
                crate::runtime::OutputState::Available(value) => Some(value),
                _ => None,
            },
        }
    };
    Ok(Captured {
        waiting,
        sample: DisplaySample {
            revision: runtime.display_revision(node),
            sources,
            over_budget: false,
            producer_run: if output.port == OutputPort::Data {
                runtime.value_run(node)
            } else {
                runtime.run_of(node)
            }
            .cloned(),
            value,
            epochs: epochs.into_iter().collect(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wes_core::{Data, Provenance, Shape, Value};
    fn sample(n: i64) -> DisplaySample {
        DisplaySample {
            revision: n as u64,
            sources: vec![],
            over_budget: false,
            producer_run: None,
            value: Some(Value::new(Shape::Unknown, Data::Int(n), Provenance::default()).unwrap()),
            epochs: vec![],
        }
    }
    fn pending_source() -> (Workspace, crate::runtime::Run) {
        let mut workspace =
            Workspace::local(crate::providers::LocalScope::new("display-fixture").unwrap());
        let parsed = wes_language::parse(&wes_language::SourceText::new(
            "display-fixture",
            ":calc { return 1; } > source",
        ));
        assert!(parsed.diagnostics.is_empty());
        let crate::workspace::Preparation::Change(change) =
            workspace.prepare(&parsed.script.statements[0]).unwrap()
        else {
            panic!("change")
        };
        workspace.commit(change).unwrap();
        let run = workspace
            .start(std::time::Duration::ZERO)
            .into_iter()
            .find_map(|effect| {
                if let crate::runtime::Effect::Spawn(ticket) = effect {
                    Some(ticket.run)
                } else {
                    None
                }
            })
            .unwrap();
        assert!(workspace.enter(&run));
        (workspace, run)
    }
    #[tokio::test(start_paused = true)]
    async fn display_reads_and_observations_keep_error_cancel_and_data_ports_separate() {
        use crate::{
            graph::NodeState,
            runtime::{Effect, Outcome, OutputState},
        };
        for selected in [OutputPort::Data, OutputPort::Error, OutputPort::Cancel] {
            let (mut workspace, run) = pending_source();
            let mut displays = Displays::default();
            for port in [OutputPort::Data, OutputPort::Error, OutputPort::Cancel] {
                assert!(
                    displays
                        .read_output(
                            &workspace,
                            &OutputRef {
                                node: run.node().clone(),
                                port,
                            }
                        )
                        .unwrap()
                        .value
                        .is_none()
                );
            }
            let error = wes_core::ErrorValue::new(
                wes_core::ErrorId::new("display-problem").unwrap(),
                "EXAMPLE",
                "Synthetic problem",
                vec![],
                None,
            )
            .unwrap();
            let outcome = match selected {
                OutputPort::Data => Outcome::Produced(sample(42).value.unwrap()),
                OutputPort::Error => Outcome::Failed(error),
                OutputPort::Cancel => Outcome::Cancelled(error),
            };
            let effects = workspace.complete(&run, outcome, std::time::Duration::ZERO);
            for effect in effects {
                if let Effect::Observe(observation) = effect {
                    displays.observe(&workspace, &observation);
                }
            }
            for port in [OutputPort::Data, OutputPort::Error, OutputPort::Cancel] {
                let output = OutputRef {
                    node: run.node().clone(),
                    port,
                };
                let read = displays.read_output(&workspace, &output).unwrap();
                if port == selected {
                    let OutputState::Available(expected) = workspace.runtime().output(&output)
                    else {
                        panic!("selected completion")
                    };
                    assert_eq!(read.value.as_ref(), Some(&expected));
                    assert_eq!(read.producer_run.as_ref(), Some(run.id()));
                } else {
                    assert!(read.value.is_none(), "wrong port {port:?}");
                }
            }
            // Private authority withdrawal is immediate even before the next cadence tick.
            let private = Value::new(
                Shape::Unknown,
                Data::Int(99),
                Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
            )
            .unwrap();
            displays.observe(
                &workspace,
                &Observation {
                    node: run.node().clone(),
                    run: Some(run.id().clone()),
                    revision: 99,
                    state: NodeState::Ready,
                    value: Some(private),
                    error: None,
                    stale_reason: None,
                    delivery: None,
                    evidence: None,
                },
            );
            assert!(
                displays
                    .entries
                    .values()
                    .all(|entry| entry.sample.value.is_none())
            );
        }
    }
    #[tokio::test(start_paused = true)]
    async fn refresh_waiting_cannot_mask_private_ancestor_withdrawal() {
        use crate::{
            runtime::{Effect, Outcome},
            workspace::Preparation,
        };
        let (mut workspace, source_run) = pending_source();
        let parsed = wes_language::parse(&wes_language::SourceText::new(
            "display-fixture",
            ":calc pure { return $source; } > child",
        ));
        let Preparation::Change(change) = workspace.prepare(&parsed.script.statements[0]).unwrap()
        else {
            panic!("child")
        };
        workspace.commit(change).unwrap();
        let public = sample(42).value.unwrap();
        let child_run = workspace
            .complete(
                &source_run,
                Outcome::Produced(public.clone()),
                std::time::Duration::ZERO,
            )
            .into_iter()
            .find_map(|effect| match effect {
                Effect::Spawn(ticket) => Some(ticket.run),
                _ => None,
            })
            .unwrap();
        assert!(workspace.enter(&child_run));
        workspace.complete(
            &child_run,
            Outcome::Produced(public),
            std::time::Duration::ZERO,
        );
        let mut displays = Displays::default();
        assert!(
            displays
                .read(&workspace, child_run.node())
                .unwrap()
                .value
                .is_some()
        );
        let parsed = wes_language::parse(&wes_language::SourceText::new(
            "display-fixture",
            ":refresh $source",
        ));
        let Preparation::Meta(meta) = workspace.prepare(&parsed.script.statements[0]).unwrap()
        else {
            panic!("refresh")
        };
        let control = workspace.prepare_control(meta).unwrap();
        let refreshed = workspace
            .apply_control(control, std::time::Duration::ZERO)
            .unwrap()
            .effects
            .into_iter()
            .find_map(|effect| match effect {
                Effect::Spawn(ticket) => Some(ticket.run),
                _ => None,
            })
            .unwrap();
        assert!(
            displays
                .read(&workspace, child_run.node())
                .unwrap()
                .value
                .is_none()
        );
        assert!(workspace.enter(&refreshed));
        let private = Value::new(
            Shape::Unknown,
            Data::Int(99),
            Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
        )
        .unwrap();
        workspace.complete(
            &refreshed,
            Outcome::Produced(private),
            std::time::Duration::ZERO,
        );
        assert!(matches!(
            displays.read(&workspace, child_run.node()),
            Err(SessionError::Authority)
        ));
        assert!(
            !displays
                .entries
                .contains_key(&OutputRef::data(child_run.node().clone()))
        );
    }
    #[tokio::test(start_paused = true)]
    async fn a_fresh_display_during_refresh_reads_the_completed_producing_run() {
        use crate::{runtime::Outcome, workspace::Preparation};
        let (mut workspace, run) = pending_source();
        let expected = sample(42).value.unwrap();
        workspace.complete(
            &run,
            Outcome::Produced(expected.clone()),
            std::time::Duration::ZERO,
        );
        let parsed = wes_language::parse(&wes_language::SourceText::new(
            "display-fixture",
            ":refresh $source",
        ));
        let Preparation::Meta(meta) = workspace.prepare(&parsed.script.statements[0]).unwrap()
        else {
            panic!("refresh")
        };
        let control = workspace.prepare_control(meta).unwrap();
        workspace
            .apply_control(control, std::time::Duration::ZERO)
            .unwrap();
        assert_ne!(workspace.runtime().run_of(run.node()), Some(run.id()));
        let read = Displays::default().read(&workspace, run.node()).unwrap();
        assert_eq!(read.value.as_ref(), Some(&expected));
        assert_eq!(read.producer_run.as_ref(), Some(run.id()));
    }
    #[tokio::test(start_paused = true)]
    async fn display_retention_charge_accepts_a_normal_500_record_window() {
        let row = Data::Record(
            [
                ("container".into(), Data::Text("a".repeat(64).into())),
                ("sequence".into(), Data::Int(1)),
                ("text".into(), Data::Text("synthetic log event".into())),
                ("stream".into(), Data::Text("stdout".into())),
                ("received_at_ns".into(), Data::Int(1)),
                ("timestamp_ns".into(), Data::Option(None)),
                ("partial".into(), Data::Bool(false)),
                ("lossy".into(), Data::Bool(false)),
                ("line_truncated".into(), Data::Bool(false)),
            ]
            .into(),
        );
        let value = Value::new(
            Shape::Unknown,
            Data::List(vec![row; 500]),
            Provenance::default(),
        )
        .unwrap();
        assert!(crate::value_size::value_charge(&value, 512 * 1024).is_none());
        let now = Instant::now();
        let mut entry = Entry {
            expires: now + TTL,
            next: now,
            sample: sample(0),
        };
        let mut next = sample(1);
        next.value = Some(value);
        Displays::sample(&mut entry, next, now);
        assert!(!entry.sample.over_budget);
        assert!(entry.sample.value.is_some());
    }
    #[tokio::test(start_paused = true)]
    async fn display_retains_one_sample_at_cadence_expires_demand_and_refuses_large_values() {
        let now = Instant::now();
        let mut entry = Entry {
            expires: now + TTL,
            next: now,
            sample: sample(0),
        };
        Displays::sample(&mut entry, sample(1), now);
        for n in 2..1000 {
            Displays::sample(&mut entry, sample(n), now);
        }
        assert_eq!(entry.sample.value.as_ref().unwrap().data(), &Data::Int(1));
        tokio::time::advance(PERIOD).await;
        Displays::sample(&mut entry, sample(1000), Instant::now());
        assert_eq!(
            entry.sample.value.as_ref().unwrap().data(),
            &Data::Int(1000)
        );
        tokio::time::advance(PERIOD).await;
        let large = Value::new(
            Shape::Unknown,
            Data::Bytes(vec![0; max_retained_charge() as usize + 1].into()),
            Provenance::default(),
        )
        .unwrap();
        Displays::sample(
            &mut entry,
            DisplaySample {
                revision: 1001,
                sources: vec![],
                over_budget: false,
                producer_run: None,
                value: Some(large),
                epochs: vec![],
            },
            Instant::now(),
        );
        assert!(entry.sample.over_budget);
        assert!(entry.sample.value.is_none());
        let mut displays = Displays::default();
        displays
            .entries
            .insert(OutputRef::data(NodeId::new("test").unwrap()), entry);
        tokio::time::advance(TTL).await;
        displays.expire();
        assert!(displays.entries.is_empty());
        assert!(displays.deadline().is_none());
    }
}
