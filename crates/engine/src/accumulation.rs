//! Stateful ordered history. Data and checkpoint are one immutable publication candidate.
use crate::{
    driver::CancellationToken,
    graph::{NodeId, OutputRef},
    plan::{Input, MetaTask},
    runtime::{Outcome, Run, Runtime, RuntimeCode},
    tasks::BoundTask,
};
use indexmap::IndexMap;
use wes_core::{Data, Primitive, Provenance, RecordShape, Shape, Value, capability::Typing};
use wes_language::{Diagnostic, Span};

fn event_bytes() -> u64 {
    wes_budgets::get("accumulate.event.bytes") as u64
}
fn total_bytes() -> u64 {
    wes_budgets::get("accumulate.bytes") as u64
}
fn max_items() -> usize {
    wes_budgets::get("accumulate.items") as usize
}

#[derive(Clone, Debug)]
pub struct BoundAccumulation {
    input: OutputRef,
    limit: usize,
    drop_oldest: bool,
    capture: Option<Capture>,
}
#[derive(Clone, Debug)]
struct Capture {
    previous: Option<Value>,
    delivery: Option<(String, String, u64)>,
}
impl BoundAccumulation {
    pub(crate) fn bind(
        task: MetaTask,
        pipe: Option<&OutputRef>,
        span: Span,
    ) -> Result<Self, Diagnostic> {
        let invalid = |message| Diagnostic::error("ACC001", span, message);
        let Some(input) = pipe else {
            return Err(invalid("accumulate requires an ordered event pipeline"));
        };
        if task.subjects.len() != 1 || task.subjects[0].dependency() != Some(input) {
            return Err(invalid("accumulate consumes the preceding ordered event"));
        }
        let Some(Input::Literal(value)) = task.inputs.get("limit") else {
            return Err(invalid("limit must be a literal integer from 1 to 100000"));
        };
        let Data::Int(limit) = value.data() else {
            return Err(invalid("limit must be an integer"));
        };
        if !(1..=max_items() as i64).contains(limit) {
            return Err(invalid("limit must be from 1 to 100000"));
        }
        let drop_oldest = match task.inputs.get("overflow") {
            None => false,
            Some(Input::Literal(v)) if v.data() == &Data::Text("error".into()) => false,
            Some(Input::Literal(v)) if v.data() == &Data::Text("drop-oldest".into()) => true,
            _ => return Err(invalid("overflow must be literal error or drop-oldest")),
        };
        Ok(Self {
            input: input.clone(),
            limit: *limit as usize,
            drop_oldest,
            capture: None,
        })
    }
    pub(crate) fn dependency(&self) -> &OutputRef {
        &self.input
    }
    pub(crate) fn predicted_typing(&self, origin: impl Fn(&OutputRef) -> Option<Typing>) -> Typing {
        let input = origin(&self.input);
        Typing {
            shape: envelope_shape(input.as_ref().map_or(Shape::Unknown, |t| t.shape.clone())),
            provenance: input.map_or_else(Provenance::default, |t| t.provenance),
        }
    }
    pub(crate) fn capture(&mut self, runtime: &Runtime<BoundTask>, run: &Run) {
        let delivery = runtime.ordered_delivery(run.node());
        // The scheduler resets delivery numbers only when entering a new source epoch.
        // Node lifetime runs are not accumulation history: explicit source refresh starts
        // a new history, while missing state after the first delivery remains an error.
        let first = delivery
            .as_ref()
            .is_some_and(|(_, _, sequence)| *sequence == 1);
        self.capture = Some(Capture {
            previous: if first {
                None
            } else {
                runtime.value_of(run.node()).cloned()
            },
            delivery: delivery
                .map(|(root, epoch, sequence)| (root.to_string(), epoch.to_string(), sequence)),
        });
    }
    pub(crate) fn captured_policy(&self) -> wes_core::flow::FlowPolicy {
        self.capture
            .as_ref()
            .and_then(|c| c.previous.as_ref())
            .map_or_else(Default::default, |v| v.provenance().policy().clone())
    }
    fn policy(&self) -> &'static str {
        if self.drop_oldest {
            "drop-oldest"
        } else {
            "error"
        }
    }
    pub(crate) fn evaluate(
        &self,
        inputs: &IndexMap<NodeId, Value>,
        token: &CancellationToken,
    ) -> Outcome {
        match self.candidate(inputs, token) {
            Ok(value) => Outcome::Produced(value),
            Err(error) => error,
        }
    }
    fn candidate(
        &self,
        inputs: &IndexMap<NodeId, Value>,
        token: &CancellationToken,
    ) -> Result<Value, Outcome> {
        cancelled(token)?;
        let input = inputs
            .get(&self.input.node)
            .ok_or_else(|| fail("ACC002", "The captured event is unavailable."))?;
        let capture = self
            .capture
            .as_ref()
            .ok_or_else(|| fail("ACC002", "Ordered delivery capture is unavailable."))?;
        let (source, epoch, sequence) = capture.delivery.as_ref().ok_or_else(|| fail("ACC002", "No current ordered delivery. Restore is held; create a new ordered pipeline to continue."))?;
        let sequence =
            i64::try_from(*sequence).map_err(|_| fail("ACC003", "Event sequence exhausted."))?;
        if !input.data().is_inline()
            || crate::value_size::value_charge(input, event_bytes()).is_none()
        {
            return Err(fail(
                "ACC004",
                "Each accumulated event must be materialized and fit the 1 MiB event budget.",
            ));
        }
        let mut items = vec![];
        let mut dropped = 0i64;
        let mut provenance = input.provenance().clone();
        if let Some(previous) = &capture.previous {
            if crate::value_size::value_charge(previous, total_bytes()).is_none() {
                return Err(lost());
            }
            let Data::Record(record) = previous.data() else {
                return Err(lost());
            };
            let Some(Data::Record(checkpoint)) = record.get("checkpoint") else {
                return Err(lost());
            };
            let same = |key: &str, value: Data| checkpoint.get(key) == Some(&value);
            if !same("version", Data::Int(1))
                || !same("limit", Data::Int(self.limit as i64))
                || !same("overflow", Data::Text(self.policy().into()))
            {
                return Err(lost());
            }
            if !same("source", Data::Text(source.as_str().into()))
                || !same("epoch", Data::Text(epoch.as_str().into()))
            {
                return Err(fail(
                    "ACC005",
                    "The checkpoint belongs to another source run. Refresh the stream source to start a new empty history; histories from different runs cannot be mixed.",
                ));
            }
            let (Some(Data::Int(seen)), Some(Data::Int(old_dropped)), Some(Data::List(old_items))) = (
                checkpoint.get("sequence"),
                checkpoint.get("dropped"),
                record.get("items"),
            ) else {
                return Err(lost());
            };
            if *seen < 1
                || *old_dropped < 0
                || old_items.len() > self.limit
                || (*seen as u64).checked_sub(*old_dropped as u64) != Some(old_items.len() as u64)
            {
                return Err(lost());
            }
            provenance = provenance.inheriting(previous.provenance());
            if sequence == *seen {
                return Ok(previous.with_provenance(provenance));
            }
            if seen.checked_add(1) != Some(sequence) {
                return Err(fail(
                    "ACC003",
                    "An event gap or backward delivery prevents complete accumulation.",
                ));
            }
            dropped = *old_dropped;
            items = old_items.clone();
        } else if sequence != 1 {
            return Err(lost());
        }
        if items.len() == self.limit {
            if !self.drop_oldest {
                return Err(fail(
                    "ACC004",
                    "Accumulation item limit reached. Explicit overflow:drop-oldest permits bounded history truncation.",
                ));
            }
            items.remove(0);
            dropped += 1;
        }
        items.push(input.data().clone());
        // Byte overflow is always an error: row truncation never hides an oversized event/history.
        let checkpoint = Data::Record(
            [
                ("version".into(), Data::Int(1)),
                ("source".into(), Data::Text(source.as_str().into())),
                ("epoch".into(), Data::Text(epoch.as_str().into())),
                ("sequence".into(), Data::Int(sequence)),
                ("limit".into(), Data::Int(self.limit as i64)),
                ("overflow".into(), Data::Text(self.policy().into())),
                ("dropped".into(), Data::Int(dropped)),
            ]
            .into(),
        );
        let value = Value::new(
            envelope_shape(Shape::Unknown),
            Data::Record(
                [
                    ("items".into(), Data::List(items)),
                    ("checkpoint".into(), checkpoint),
                ]
                .into(),
            ),
            provenance,
        )
        .expect("accumulation envelope matches schema");
        if crate::value_size::value_charge(&value, total_bytes()).is_none() {
            return Err(fail(
                "ACC004",
                "Accumulation exceeds the 8 MiB retained value budget.",
            ));
        }
        cancelled(token)?;
        Ok(value)
    }
}
fn envelope_shape(item: Shape) -> Shape {
    let int = Shape::Primitive(Primitive::Int);
    let text = Shape::Primitive(Primitive::Text);
    Shape::Record(
        RecordShape::new(
            "Accumulation",
            [
                ("items".into(), Shape::List(Box::new(item))),
                (
                    "checkpoint".into(),
                    Shape::Record(
                        RecordShape::new(
                            "AccumulationCheckpoint",
                            [
                                ("version".into(), int.clone()),
                                ("source".into(), text.clone()),
                                ("epoch".into(), text.clone()),
                                ("sequence".into(), int.clone()),
                                ("limit".into(), int.clone()),
                                ("overflow".into(), text),
                                ("dropped".into(), int),
                            ],
                        )
                        .expect("unique checkpoint fields"),
                    ),
                ),
            ],
        )
        .expect("unique envelope fields"),
    )
}
fn fail(code: &str, message: &str) -> Outcome {
    Outcome::Failed(
        wes_core::ErrorValue::new(
            wes_core::ErrorId::new(uuid::Uuid::new_v4().to_string()).unwrap(),
            code,
            message,
            vec![],
            None,
        )
        .unwrap(),
    )
}
fn lost() -> Outcome {
    fail(
        "ACC006",
        "Accumulation checkpoint is unavailable or incompatible. State was not silently reset. Refresh the stream source to start a new empty history.",
    )
}
fn cancelled(token: &CancellationToken) -> Result<(), Outcome> {
    if token.is_cancelled() {
        Err(Outcome::Cancelled(
            RuntimeCode::Cancelled.error("Accumulation was cancelled.", None),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(limit: usize, drop_oldest: bool) -> BoundAccumulation {
        BoundAccumulation {
            input: OutputRef::data(NodeId::new("event").unwrap()),
            limit,
            drop_oldest,
            capture: Some(Capture {
                previous: None,
                delivery: Some(("source".into(), "epoch-1".into(), 1)),
            }),
        }
    }
    fn event(n: i64) -> Value {
        Value::new(
            Shape::Primitive(Primitive::Int),
            Data::Int(n),
            Provenance::default(),
        )
        .unwrap()
    }
    fn step(a: &BoundAccumulation, value: Value) -> Result<Value, Outcome> {
        a.candidate(
            &[(a.input.node.clone(), value)].into(),
            &CancellationToken::new(),
        )
    }
    fn next(a: &mut BoundAccumulation, value: Value, seq: u64) {
        let c = a.capture.as_mut().unwrap();
        c.previous = Some(value);
        c.delivery.as_mut().unwrap().2 = seq;
    }
    fn record(value: &Value) -> &IndexMap<String, Data> {
        let Data::Record(r) = value.data() else {
            panic!()
        };
        r
    }
    fn code(result: Result<Value, Outcome>, expected: &str) {
        assert!(matches!(result, Err(Outcome::Failed(e)) if e.code() == expected));
    }
    #[test]
    fn duplicates_are_noops_equal_content_is_new_and_drop_count_is_exact() {
        let mut a = fixture(2, true);
        let first = step(&a, event(7)).unwrap();
        next(&mut a, first.clone(), 1);
        assert_eq!(step(&a, event(7)).unwrap(), first);
        next(&mut a, first, 2);
        let second = step(&a, event(7)).unwrap();
        assert_eq!(
            record(&second)["items"],
            Data::List(vec![Data::Int(7), Data::Int(7)])
        );
        next(&mut a, second, 3);
        let third = step(&a, event(9)).unwrap();
        assert_eq!(
            record(&third)["items"],
            Data::List(vec![Data::Int(7), Data::Int(9)])
        );
        let Data::Record(k) = &record(&third)["checkpoint"] else {
            panic!()
        };
        assert_eq!(k["sequence"], Data::Int(3));
        assert_eq!(k["dropped"], Data::Int(1));
    }
    #[test]
    fn gaps_regression_epochs_and_missing_or_changed_checkpoint_are_explicit() {
        let mut a = fixture(10, false);
        let first = step(&a, event(1)).unwrap();
        next(&mut a, first.clone(), 3);
        code(step(&a, event(2)), "ACC003");
        next(&mut a, first.clone(), 0);
        code(step(&a, event(2)), "ACC003");
        next(&mut a, first.clone(), 2);
        a.capture.as_mut().unwrap().delivery.as_mut().unwrap().1 = "epoch-2".into();
        code(step(&a, event(2)), "ACC005");
        a.capture.as_mut().unwrap().previous = None;
        code(step(&a, event(2)), "ACC006");
        a.capture.as_mut().unwrap().previous = Some(first);
        a.limit = 20;
        code(step(&a, event(2)), "ACC006");
    }
    #[test]
    fn list_event_is_one_item_and_bounds_fail_without_advancing() {
        let mut a = fixture(1, false);
        let list = Value::new(
            Shape::List(Box::new(Shape::Primitive(Primitive::Int))),
            Data::List(vec![Data::Int(1), Data::Int(2)]),
            Provenance::default(),
        )
        .unwrap();
        let first = step(&a, list.clone()).unwrap();
        assert_eq!(
            record(&first)["items"],
            Data::List(vec![list.data().clone()])
        );
        next(&mut a, first.clone(), 2);
        code(step(&a, event(2)), "ACC004");
        assert_eq!(a.capture.as_ref().unwrap().previous.as_ref(), Some(&first));
        let huge = Value::new(
            Shape::Primitive(Primitive::Text),
            Data::Text("x".repeat(event_bytes() as usize).into()),
            Provenance::default(),
        )
        .unwrap();
        code(step(&fixture(2, true), huge), "ACC004");
    }
    #[test]
    fn retained_byte_limit_cannot_be_evaded_by_drop_oldest() {
        let mut a = fixture(100, true);
        let value = Value::new(
            Shape::Primitive(Primitive::Text),
            Data::Text("x".repeat(100_000).into()),
            Provenance::default(),
        )
        .unwrap();
        let mut limited = false;
        for sequence in 1..100 {
            match step(&a, value.clone()) {
                Ok(v) => next(&mut a, v, sequence + 1),
                Err(Outcome::Failed(e)) => {
                    assert_eq!(e.code(), "ACC004");
                    limited = true;
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(limited);
    }
    #[test]
    fn dropped_private_history_keeps_checkpoint_private() {
        let mut a = fixture(1, true);
        let private = event(1).with_provenance(
            Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
        );
        let first = step(&a, private).unwrap();
        next(&mut a, first, 2);
        let value = step(&a, event(2)).unwrap();
        assert!(value.provenance().policy().is_private());
        assert_eq!(record(&value)["items"], Data::List(vec![Data::Int(2)]));
    }
    #[test]
    fn cancellation_does_not_mutate_candidate_checkpoint() {
        let mut a = fixture(2, true);
        let first = step(&a, event(1)).unwrap();
        next(&mut a, first.clone(), 2);
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            a.candidate(&[(a.input.node.clone(), event(2))].into(), &token),
            Err(Outcome::Cancelled(_))
        ));
        assert_eq!(a.capture.as_ref().unwrap().previous.as_ref(), Some(&first));
    }
}
