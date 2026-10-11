//! Bounded run evidence. Providers supply domain values; this module knows no wire protocol.
use crate::{
    graph::NodeId,
    runtime::{Run, RunId},
    value_size::value_charge,
};
use indexmap::IndexMap;
use std::{
    sync::{Arc, Mutex},
    time::Instant,
};
use wes_core::{Data, Primitive, Provenance, RecordShape, Shape, Value};

pub fn max_trace_bytes() -> u64 {
    wes_budgets::get("trace.bytes") as u64
}
fn max_traces() -> usize {
    wes_budgets::get("trace.count") as usize
}
fn max_events() -> usize {
    wes_budgets::get("trace.events") as usize
}

/// Construct a materialized typed record without registering protocol-specific core primitives.
pub fn record(
    name: &str,
    fields: impl IntoIterator<Item = (String, Value)>,
    provenance: Provenance,
) -> Value {
    let fields: IndexMap<_, _> = fields.into_iter().collect();
    let shape = Shape::Record(
        RecordShape::new(
            name,
            fields.iter().map(|(k, v)| (k.clone(), v.shape().clone())),
        )
        .expect("unique record fields"),
    );
    let provenance = fields
        .values()
        .fold(provenance, |p, v| p.inheriting(v.provenance()));
    Value::new(
        shape,
        Data::Record(
            fields
                .into_iter()
                .map(|(k, v)| (k, v.data().clone()))
                .collect(),
        ),
        provenance,
    )
    .expect("record shape")
}
pub fn text(value: impl Into<String>) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text(value.into().into()),
        Provenance::default(),
    )
    .expect("text")
}
pub fn integer(value: i64) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(value),
        Provenance::default(),
    )
    .expect("integer")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceRecord {
    pub node: NodeId,
    pub run: RunId,
    pub value: Value,
}
impl TraceRecord {
    pub fn validate(&self) -> bool {
        if self.node.as_str().len() > 256
            || self.run.as_str().len() > 256
            || value_charge(&self.value, max_trace_bytes()).is_none()
            || self.value.provenance().policy().is_confidential()
            || self.value.provenance().policy().is_unknown()
        {
            return false;
        }
        let Data::Record(fields) = self.value.data() else {
            return false;
        };
        if fields.len() != 8
            || fields.get("schema") != Some(&Data::Int(1))
            || !matches!(fields.get("profile"), Some(Data::Text(p)) if !p.is_empty() && p.len() <= 64)
            || !matches!(fields.get("dropped"), Some(Data::Int(n)) if *n >= 0)
        {
            return false;
        }
        let Some(Data::List(events)) = fields.get("events") else {
            return false;
        };
        if events.len() > max_events() {
            return false;
        }
        let mut previous = 0;
        for event in events {
            let Data::Record(event) = event else {
                return false;
            };
            let Some(Data::Int(elapsed)) = event.get("elapsedMs") else {
                return false;
            };
            if *elapsed < previous
                || event.len() != 3
                || !event.contains_key("details")
                || !matches!(event.get("kind"), Some(Data::Text(k)) if !k.is_empty() && k.len()<=64)
            {
                return false;
            }
            previous = *elapsed;
        }
        fields.get("node") == Some(&Data::Text(self.node.to_string().into()))
            && fields.get("run") == Some(&Data::Text(self.run.to_string().into()))
            && matches!(fields.get("state"), Some(Data::Text(s)) if matches!(s.as_ref(), "completed" | "failed" | "cancelled"))
            && fields.get("persistence") == Some(&Data::Text("recorded".into()))
    }
}

#[derive(Clone, Debug, Default)]
pub struct Traces(Arc<Mutex<IndexMap<RunId, Arc<Mutex<Entry>>>>>);
impl Traces {
    pub(crate) fn retire(&self, nodes: &std::collections::BTreeSet<NodeId>) {
        self.0
            .lock()
            .expect("trace lock")
            .retain(|_, entry| !nodes.contains(&entry.lock().expect("trace entry lock").node));
    }
}
#[derive(Debug)]
struct Entry {
    node: NodeId,
    run: RunId,
    profile: String,
    state: String,
    persistence: String,
    provenance: Provenance,
    events: Vec<Value>,
    charged: u64,
    dropped: i64,
    restored: Option<Value>,
}
impl Entry {
    fn snapshot(&self) -> Value {
        if let Some(value) = &self.restored {
            return value.clone();
        }
        record(
            "Trace",
            [
                ("schema".into(), integer(1)),
                ("node".into(), text(self.node.to_string())),
                ("run".into(), text(self.run.to_string())),
                ("profile".into(), text(&self.profile)),
                ("state".into(), text(&self.state)),
                ("persistence".into(), text(&self.persistence)),
                ("dropped".into(), integer(self.dropped)),
                (
                    "events".into(),
                    Value::new(
                        Shape::List(Box::new(Shape::Unknown)),
                        Data::List(self.events.iter().map(|v| v.data().clone()).collect()),
                        self.provenance.clone(),
                    )
                    .expect("events"),
                ),
            ],
            self.provenance.clone(),
        )
    }
}
impl Traces {
    pub fn begin(&self, run: &Run, profile: &str, provenance: Provenance) -> Option<TraceSink> {
        let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if profile.is_empty()
            || profile.len() > 64
            || run.node().as_str().len() > 256
            || run.id().as_str().len() > 256
            || entries.contains_key(run.id())
        {
            return None;
        }
        let mut provenance = Provenance::default().with_policy(provenance.policy());
        if provenance.policy().is_unknown() {
            provenance = provenance
                .clone()
                .with_policy(&provenance.policy().clone().private());
        }
        if entries.len() >= max_traces() {
            let old = entries.iter().find_map(|(id, e)| {
                (e.lock().unwrap_or_else(|p| p.into_inner()).state != "running").then(|| id.clone())
            })?;
            entries.shift_remove(&old);
        }
        let entry = Arc::new(Mutex::new(Entry {
            node: run.node().clone(),
            run: run.id().clone(),
            profile: profile.into(),
            state: "running".into(),
            persistence: "memory".into(),
            provenance,
            events: vec![],
            charged: 4096,
            dropped: 0,
            restored: None,
        }));
        entries.insert(run.id().clone(), entry.clone());
        Some(TraceSink {
            entry,
            started: Instant::now(),
        })
    }
    pub fn get(&self, node: &NodeId, run: Option<&str>) -> Option<Value> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .rev()
            .find_map(|entry| {
                let entry = entry.lock().unwrap_or_else(|e| e.into_inner());
                (&entry.node == node && run.is_none_or(|r| r == entry.run.as_str()))
                    .then(|| entry.snapshot())
            })
    }
    pub fn snapshots(&self) -> Vec<Value> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|e| e.lock().unwrap_or_else(|e| e.into_inner()).snapshot())
            .collect()
    }
    pub fn restore(&self, record: &TraceRecord) -> bool {
        if !record.validate() {
            return false;
        }
        let mut entries = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = entries.get(&record.run) {
            return previous
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .snapshot()
                == record.value;
        }
        if entries.len() >= max_traces() {
            entries.shift_remove_index(0);
        }
        entries.insert(
            record.run.clone(),
            Arc::new(Mutex::new(Entry {
                node: record.node.clone(),
                run: record.run.clone(),
                profile: String::new(),
                state: "restored".into(),
                persistence: "recorded".into(),
                provenance: record.value.provenance().clone(),
                events: vec![],
                charged: 0,
                dropped: 0,
                restored: Some(record.value.clone()),
            })),
        );
        true
    }
}

#[derive(Clone, Debug)]
pub struct TraceSink {
    entry: Arc<Mutex<Entry>>,
    started: Instant,
}
impl TraceSink {
    /// The explicitly selected, immutable profile for this attempt.
    pub fn profile(&self) -> String {
        self.entry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .profile
            .clone()
    }
    pub fn emit(&self, kind: &str, details: Value) {
        let mut entry = self.entry.lock().unwrap_or_else(|e| e.into_inner());
        if entry.state != "running" {
            return;
        }
        // Policy is inherited even when a payload is too large to retain.
        entry.provenance = entry
            .provenance
            .clone()
            .with_policy(details.provenance().policy());
        if entry.provenance.policy().is_unknown() {
            entry.provenance = entry
                .provenance
                .clone()
                .with_policy(&entry.provenance.policy().clone().private());
        }
        let charge = value_charge(&details, max_trace_bytes());
        if kind.is_empty()
            || kind.len() > 64
            || entry.events.len() >= max_events()
            || charge.is_none_or(|c| entry.charged.saturating_add(c + 2048) > max_trace_bytes() / 2)
        {
            entry.dropped = entry.dropped.saturating_add(1);
            return;
        }
        entry.charged += charge.expect("checked") + 2048;
        entry.events.push(record(
            "Observation",
            [
                ("kind".into(), text(kind)),
                (
                    "elapsedMs".into(),
                    integer(self.started.elapsed().as_millis().min(i64::MAX as u128) as i64),
                ),
                ("details".into(), details),
            ],
            Provenance::default(),
        ));
    }
    pub(crate) fn finish(&self, state: &str) {
        self.entry.lock().unwrap_or_else(|e| e.into_inner()).state = state.into();
    }
    pub(crate) fn persistence(&self, state: &str) {
        self.entry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .persistence = state.into();
    }
    pub(crate) fn persistent_record(&self) -> Option<TraceRecord> {
        let mut entry = self.entry.lock().unwrap_or_else(|e| e.into_inner());
        let old = std::mem::replace(&mut entry.persistence, "recorded".into());
        let record = TraceRecord {
            node: entry.node.clone(),
            run: entry.run.clone(),
            value: entry.snapshot(),
        };
        entry.persistence = old;
        record.validate().then_some(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{Effect, ExecutionTraits, Runtime};
    fn run() -> Run {
        let mut runtime = Runtime::new();
        runtime
            .add(
                (),
                [],
                ExecutionTraits {
                    pure: false,
                    repeatable: true,
                    bounded: true,
                },
            )
            .unwrap();
        runtime
            .start(std::time::Duration::ZERO)
            .into_iter()
            .find_map(|e| {
                if let Effect::Spawn(t) = e {
                    Some(t.run)
                } else {
                    None
                }
            })
            .unwrap()
    }
    #[test]
    fn generic_profiles_bounds_policy_and_restore() {
        let traces = Traces::default();
        let run = run();
        let sink = traces.begin(&run, "binary", Provenance::default()).unwrap();
        for _ in 0..100 {
            sink.emit("binary.field", text("abc"));
        }
        sink.finish("completed");
        let record = sink.persistent_record().unwrap();
        assert!(record.validate());
        let Data::Record(fields) = record.value.data() else {
            panic!()
        };
        assert!(matches!(fields.get("dropped"),Some(Data::Int(n)) if *n>0));
        assert!(value_charge(&record.value, max_trace_bytes()).is_some());
        let restored = Traces::default();
        assert!(restored.restore(&record));
        assert!(restored.restore(&record));
        assert_eq!(
            restored.get(run.node(), Some(run.id().as_str())),
            Some(record.value)
        );
        let private = Traces::default()
            .begin(
                &run,
                "other",
                Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
            )
            .unwrap();
        private.finish("failed");
        assert!(private.persistent_record().is_none());
        let unknown = Traces::default()
            .begin(
                &run,
                "other",
                Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().unknown()),
            )
            .unwrap();
        unknown.finish("cancelled");
        assert!(unknown.persistent_record().is_none());
    }
    #[test]
    fn active_capacity_is_fail_closed_and_completed_entries_are_evicted() {
        let traces = Traces::default();
        let mut sinks = vec![];
        for _ in 0..max_traces() {
            sinks.push(
                traces
                    .begin(&run(), "synthetic", Provenance::default())
                    .unwrap(),
            );
        }
        let next = run();
        assert!(
            traces
                .begin(&next, "synthetic", Provenance::default())
                .is_none()
        );
        sinks[0].finish("cancelled");
        assert!(
            traces
                .begin(&next, "synthetic", Provenance::default())
                .is_some()
        );
        assert!(
            traces
                .begin(&next, "synthetic", Provenance::default())
                .is_none()
        );
        assert_eq!(traces.snapshots().len(), max_traces());
    }
    #[test]
    fn dropped_private_payload_taints_trace_and_late_events_do_not_change_it() {
        let traces = Traces::default();
        let run = run();
        let sink = traces
            .begin(&run, "synthetic", Provenance::default())
            .unwrap();
        let private = text("x".repeat(max_trace_bytes() as usize)).with_provenance(
            Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
        );
        sink.emit("large", private);
        sink.finish("completed");
        assert!(sink.persistent_record().is_none());
        let before = traces.get(run.node(), None);
        sink.emit("late", text("late"));
        assert_eq!(traces.get(run.node(), None), before);
    }
}
