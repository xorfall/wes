//! Fixed native operators. They consume captured values and never look up workspace names.
use crate::{
    driver::CancellationToken,
    graph::{NodeId, OutputRef},
    runtime::{Outcome, RuntimeCode},
};
use indexmap::IndexMap;
use wes_core::{Data, Value};

#[derive(Clone, Debug)]
pub enum Operation {
    Limit(u64),
    Skip(u64),
    Filter { field: Vec<String>, equals: Data },
    Condition(OutputRef),
    Project(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct BoundOperator {
    pub input: OutputRef,
    pub operation: Operation,
    pub delivery: Option<u64>,
}
impl BoundOperator {
    pub(crate) fn limit(&self) -> Option<u64> {
        match self.operation {
            Operation::Limit(n) => Some(n),
            _ => None,
        }
    }
    pub(crate) fn requires_events(&self) -> bool {
        matches!(self.operation, Operation::Limit(_) | Operation::Skip(_))
    }
    pub fn dependencies(&self) -> impl Iterator<Item = OutputRef> + '_ {
        std::iter::once(self.input.clone()).chain(match &self.operation {
            Operation::Condition(reference) => Some(reference.clone()),
            _ => None,
        })
    }
    pub(crate) fn evaluate(
        &self,
        inputs: &IndexMap<NodeId, Value>,
        token: &CancellationToken,
    ) -> Outcome {
        let fail = |message| Outcome::Failed(RuntimeCode::ExecutionFailed.error(message, None));
        if token.is_cancelled() {
            return Outcome::Cancelled(
                RuntimeCode::Cancelled.error("Stream operation cancelled.", None),
            );
        }
        let Some(input) = inputs.get(&self.input.node) else {
            return fail("Captured stream input is unavailable.");
        };
        if crate::value_size::value_charge(input, 8 * 1024 * 1024).is_none() {
            return fail("Native stream input exceeds the 8 MiB logical budget.");
        }
        let select = |path: &[String]| {
            let mut data = input.data();
            for field in path {
                let Data::Record(record) = data else {
                    return None;
                };
                data = record.get(field)?;
            }
            Some(data)
        };
        match &self.operation {
            Operation::Limit(_) => Outcome::Produced(input.clone()),
            Operation::Skip(n) => match self.delivery {
                Some(sequence) if sequence <= *n => Outcome::Skipped,
                Some(_) => Outcome::Produced(input.clone()),
                None => fail("An ordered delivery is required for skip."),
            },
            Operation::Condition(reference) => match inputs.get(&reference.node).map(Value::data) {
                Some(Data::Bool(true)) => Outcome::Produced(input.clone()),
                Some(Data::Bool(false)) => Outcome::Skipped,
                _ => fail("A branch predicate must produce Bool."),
            },
            Operation::Filter { field, equals } => match select(field) {
                Some(value) if value == equals => Outcome::Produced(input.clone()),
                Some(_) => Outcome::Skipped,
                None => fail("The stream filter field is unavailable."),
            },
            Operation::Project(path) => match select(path) {
                Some(data) => Outcome::Produced(
                    Value::new(
                        wes_core::Shape::Unknown,
                        data.clone(),
                        input.provenance().clone(),
                    )
                    .expect("Unknown accepts data"),
                ),
                None => fail("The stream projection field is unavailable."),
            },
        }
    }
}

#[cfg(test)]
mod shared_payload_tests {
    use super::*;
    use std::sync::Arc;
    use wes_core::{Provenance, Shape};

    #[test]
    fn native_projection_shares_text_and_byte_allocations() {
        for payload in [
            Data::Text("x".repeat(10_000).into()),
            Data::Bytes(vec![42; 10_000].into()),
        ] {
            let input = Value::new(
                Shape::Unknown,
                Data::Record(IndexMap::from([
                    ("a".into(), payload.clone()),
                    ("b".into(), Data::Int(7)),
                ])),
                Provenance::default().with_fact("fixture", "shared-payload"),
            )
            .unwrap();
            let node = NodeId::new("fixture").unwrap();
            let operator = BoundOperator {
                input: OutputRef::data(node.clone()),
                operation: Operation::Project(vec!["a".into()]),
                delivery: None,
            };
            let Outcome::Produced(output) = operator.evaluate(
                &IndexMap::from([(node, input.clone())]),
                &CancellationToken::new(),
            ) else {
                panic!("projection failed")
            };
            match (&payload, output.data()) {
                (Data::Text(a), Data::Text(b)) => assert!(Arc::ptr_eq(a, b)),
                (Data::Bytes(a), Data::Bytes(b)) => assert!(Arc::ptr_eq(a, b)),
                _ => panic!("projection changed the payload kind"),
            }
            assert_eq!(output.provenance(), input.provenance());
        }
    }
}
