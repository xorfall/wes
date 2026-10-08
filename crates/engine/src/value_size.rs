//! Shared conservative value/shape charge, not allocator accounting or an RSS guarantee.
use wes_core::{Data, Shape, Value};

fn max_nodes() -> usize {
    wes_budgets::get("value_size.nodes") as usize
}
const MAX_DEPTH: usize = 256;

pub(crate) fn value_charge(value: &Value, limit: u64) -> Option<u64> {
    let mut budget = Budget {
        charged: 256,
        visited: 0,
        limit,
    };
    charge_shell(value, &mut budget)?;
    charge_data(value.data(), &mut budget)?;
    Some(budget.charged)
}
/// Immutable attribution/declaration charge before projecting or copying a
/// record. Payload ownership is admitted separately without cloning it first.
pub(crate) fn value_shell_charge(value: &Value, limit: u64) -> Option<u64> {
    let mut budget = Budget {
        charged: 256,
        visited: 0,
        limit,
    };
    charge_shell(value, &mut budget)?;
    Some(budget.charged)
}
fn charge_shell(value: &Value, budget: &mut Budget) -> Option<()> {
    for (key, value) in value.provenance().facts() {
        budget.text(key)?;
        budget.text(value)?;
    }
    for caution in value.provenance().cautions() {
        budget.text(caution)?;
    }
    for origin in value.provenance().policy().origins() {
        budget.text(origin)?;
    }
    if let Some(meta) = value.metadata() {
        budget.add(meta.charge())?;
    }
    budget.shape(value.shape())?;
    Some(())
}
pub(crate) fn data_charge(data: &Data, limit: u64) -> Option<u64> {
    let mut budget = Budget {
        charged: 0,
        visited: 0,
        limit,
    };
    charge_data(data, &mut budget)?;
    Some(budget.charged)
}
fn charge_data(root: &Data, budget: &mut Budget) -> Option<()> {
    let mut data = vec![(root, 0)];
    while let Some((item, depth)) = data.pop() {
        budget.node(depth)?;
        match item {
            Data::Text(text) => budget.text(text)?,
            Data::Decimal(decimal) => {
                budget.add(decimal.compact_text_size_bound().saturating_mul(6))?
            }
            Data::Bytes(bytes) => budget.add((bytes.len() as u64).saturating_mul(2))?,
            Data::Iter(iter) => {
                budget.shape(iter.item_shape())?;
                budget.shape(iter.source().shape())?;
                if let Some(meta) = iter.source().metadata() {
                    budget.add(meta.charge())?;
                }
                for (key, value) in iter.source().provenance().facts() {
                    budget.text(key)?;
                    budget.text(value)?;
                }
                for caution in iter.source().provenance().cautions() {
                    budget.text(caution)?;
                }
                if let Some(arg) = iter.argument() {
                    budget.text(arg)?;
                }
                for stage in iter.stages() {
                    budget.node(depth)?;
                    match stage {
                        wes_core::IterStage::Field(name) => budget.text(name)?,
                        wes_core::IterStage::Check(c) => {
                            budget.text(&c.name)?;
                            for source in &c.packages {
                                budget.text(source)?;
                            }
                        }
                        _ => {}
                    }
                }
                budget.children(1, data.len())?;
                data.push((iter.source().data(), depth + 1));
            }
            Data::Option(Some(item)) => {
                budget.children(1, data.len())?;
                data.push((item, depth + 1));
            }
            Data::List(items) => {
                budget.children(items.len(), data.len())?;
                data.extend(items.iter().map(|item| (item, depth + 1)));
            }
            Data::Record(fields) => {
                budget.children(fields.len(), data.len())?;
                for (key, item) in fields {
                    budget.text(key)?;
                    data.push((item, depth + 1));
                }
            }
            _ => {}
        }
    }
    Some(())
}

pub(crate) fn shape_charge(shape: &Shape, limit: u64) -> Option<u64> {
    let mut budget = Budget {
        charged: 0,
        visited: 0,
        limit,
    };
    budget.shape(shape)?;
    Some(budget.charged)
}
struct Budget {
    charged: u64,
    visited: usize,
    limit: u64,
}
impl Budget {
    fn shape(&mut self, shape: &Shape) -> Option<()> {
        let mut shapes = vec![(shape, 0)];
        while let Some((shape, depth)) = shapes.pop() {
            self.node(depth)?;
            match shape {
                Shape::List(element) | Shape::Option(element) | Shape::Iter(element) => {
                    shapes.push((element, depth + 1))
                }
                Shape::Record(record) => {
                    self.text(record.name())?;
                    self.children(record.fields().len(), shapes.len())?;
                    for (key, shape) in record.fields() {
                        self.text(key)?;
                        shapes.push((shape, depth + 1));
                    }
                }
                _ => {}
            }
        }
        Some(())
    }
    fn add(&mut self, bytes: u64) -> Option<()> {
        self.charged = self.charged.checked_add(bytes)?;
        (self.charged <= self.limit).then_some(())
    }
    fn text(&mut self, text: &str) -> Option<()> {
        self.node(0)?;
        self.add((text.len() as u64).saturating_mul(6).saturating_add(64))
    }
    fn node(&mut self, depth: usize) -> Option<()> {
        self.visited += 1;
        if depth > MAX_DEPTH || self.visited > max_nodes() {
            return None;
        }
        self.add(128)
    }
    fn children(&self, count: usize, pending: usize) -> Option<()> {
        (self.visited.saturating_add(pending).saturating_add(count) <= max_nodes()).then_some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wes_core::{Decimal, Provenance, RecordShape};
    #[test]
    fn charge_includes_declared_shapes_actual_discriminants_and_provenance() {
        let base = Value::new(
            Shape::Unknown,
            Data::Text("hello".into()),
            Provenance::default(),
        )
        .unwrap();
        let amount = value_charge(&base, u64::MAX).unwrap();
        assert!(value_charge(&base, amount - 1).is_none());
        assert_eq!(value_charge(&base, amount), Some(amount));
        let attributed = base.with_provenance(
            Provenance::default()
                .with_fact("origin", "example")
                .cautioned(["unchecked:id".into()]),
        );
        assert!(value_charge(&attributed, u64::MAX).unwrap() > amount);
        let record = Value::new(
            Shape::Record(RecordShape::new("Row", [("field".into(), Shape::Unknown)]).unwrap()),
            Data::Record([("field".into(), Data::Bytes(vec![1; 100].into()))].into()),
            Provenance::default(),
        )
        .unwrap();
        assert!(value_charge(&record, u64::MAX).unwrap() > amount);
    }
    #[test]
    fn captured_metadata_is_charged_at_root_and_in_retained_iterator_sources() {
        let mut r = wes_core::contracts::ContractRegistry::new();
        r.load("types: {S: {base: Text, enum: [ready, failed]}} ")
            .unwrap();
        let base = Value::new(
            Shape::Unknown,
            Data::Text("ready".into()),
            Provenance::default(),
        )
        .unwrap();
        let captured =
            wes_core::contracts::boundary::checked_result(&r.resolve("S").unwrap(), &base, &|| {
                false
            })
            .unwrap();
        let plain = captured.with_metadata(None);
        let delta =
            value_charge(&captured, u64::MAX).unwrap() - value_charge(&plain, u64::MAX).unwrap();
        assert_eq!(delta, captured.metadata().unwrap().charge());
        assert!(value_charge(&captured, value_charge(&plain, u64::MAX).unwrap()).is_none());
        let iter = |source| {
            Data::Iter(std::sync::Arc::new(
                wes_core::IterValue::new(source, wes_core::IterMode::Lines, None, vec![]).unwrap(),
            ))
        };
        assert_eq!(
            data_charge(&iter(captured), u64::MAX).unwrap()
                - data_charge(&iter(plain), u64::MAX).unwrap(),
            delta
        );
    }
    #[test]
    fn extreme_decimal_scale_has_a_compact_charge_without_expansion() {
        for source in ["1E+2147483648", "1E-2147483647", "-0.000001", "0.0000000"] {
            let decimal: Decimal = source.parse().unwrap();
            assert!(decimal.to_string().len() as u64 <= decimal.compact_text_size_bound());
            let value = Value::new(
                Shape::Unknown,
                Data::Decimal(decimal),
                Provenance::default(),
            )
            .unwrap();
            assert!(value_charge(&value, 4096).is_some());
        }
        let decimal: Decimal = "9".repeat(10_000).parse().unwrap();
        assert!(decimal.to_string().len() as u64 <= decimal.compact_text_size_bound());
    }
    #[test]
    fn wide_and_deep_inputs_fail_before_unbounded_traversal_and_fact_work_is_counted() {
        let mut data = Data::Int(1);
        for _ in 0..MAX_DEPTH + 2 {
            data = Data::List(vec![data]);
        }
        let value = Value::new(Shape::Unknown, data, Provenance::default()).unwrap();
        assert!(value_charge(&value, u64::MAX).is_none());
        let value = Value::new(
            Shape::Unknown,
            Data::List(vec![Data::Int(1); max_nodes() + 1]),
            Provenance::default(),
        )
        .unwrap();
        assert!(value_charge(&value, u64::MAX).is_none());
        let mut budget = Budget {
            charged: 0,
            visited: max_nodes(),
            limit: u64::MAX,
        };
        assert!(budget.text("fact").is_none());
    }
}
