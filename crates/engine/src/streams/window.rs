use super::StreamError;
use crate::value_size::value_charge;
use std::{
    collections::VecDeque,
    num::{NonZeroU64, NonZeroUsize},
};
use wes_core::{Data, Provenance, Shape, Value};

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub items: NonZeroUsize,
    /// Conservative logical value charge, including shape and provenance. Not encoded bytes or RSS.
    pub bytes: NonZeroU64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            items: NonZeroUsize::new(wes_budgets::get("stream.window.items") as _).unwrap(),
            bytes: NonZeroU64::new(wes_budgets::get("stream.window.bytes") as _).unwrap(),
        }
    }
}
pub(super) fn loss_cautions(omitted: u64, rejected: u64) -> Vec<String> {
    let mut cautions = vec![];
    if omitted != 0 {
        cautions.push(format!("Stream window omitted {omitted} older items."));
    }
    if rejected != 0 {
        cautions.push(format!("Stream rejected {rejected} invalid items."));
    }
    cautions
}
#[derive(Clone)]
pub(super) struct Window {
    empty: Value,
    items: VecDeque<(Value, u64)>,
    limits: Limits,
    charged: u64,
    base: u64,
    omitted: u64,
}
impl Window {
    pub fn new(item: &Shape, attribution: Provenance, limits: Limits) -> Result<Self, StreamError> {
        if limits.items.get() > 10_000 || limits.bytes.get() > 64 * 1024 * 1024 {
            return Err(StreamError::Invalid);
        }
        // Bound metadata before recursively cloning the declared shape.
        crate::value_size::shape_charge(item, limits.bytes.get()).ok_or(StreamError::Invalid)?;
        let empty = Value::new(
            Shape::List(Box::new(item.clone())),
            Data::List(vec![]),
            attribution,
        )
        .expect("list data has list shape");
        // Reserve the maximum loss-report charge from the start. Counts change without growing
        // the advertised byte bound, even if input provenance already contains similar cautions.
        let loss_metadata = Value::new(
            Shape::Unknown,
            Data::List(vec![]),
            Provenance::default().cautioned(loss_cautions(u64::MAX, u64::MAX)),
        )
        .expect("loss metadata");
        let reserve =
            value_charge(&loss_metadata, limits.bytes.get()).ok_or(StreamError::Invalid)?;
        let charged = value_charge(
            &empty,
            limits
                .bytes
                .get()
                .checked_sub(reserve)
                .ok_or(StreamError::Invalid)?,
        )
        .ok_or(StreamError::Invalid)?
            + reserve;
        Ok(Self {
            empty,
            items: VecDeque::new(),
            limits,
            charged,
            base: charged,
            omitted: 0,
        })
    }
    pub fn push(&mut self, value: Value) -> Result<(), StreamError> {
        let attribution = self
            .empty
            .provenance()
            .clone()
            .with_policy(value.provenance().policy());
        let empty = self.empty.with_provenance(attribution);
        let old =
            value_charge(&self.empty, self.limits.bytes.get()).ok_or(StreamError::Capacity)?;
        let new = value_charge(&empty, self.limits.bytes.get()).ok_or(StreamError::Capacity)?;
        let growth = new.saturating_sub(old);
        let base = self
            .base
            .checked_add(growth)
            .filter(|n| *n <= self.limits.bytes.get())
            .ok_or(StreamError::Capacity)?;
        let charge =
            value_charge(&value, self.limits.bytes.get() - base).ok_or(StreamError::Capacity)?;
        // Check the entire admission before discarding an old item, including omission-counter space.
        let mut remove = 0;
        let mut charged = self.charged + growth;
        for (_, old_charge) in &self.items {
            if self.items.len() - remove < self.limits.items.get()
                && charged + charge <= self.limits.bytes.get()
            {
                break;
            }
            remove += 1;
            charged -= old_charge;
        }
        let omitted = self
            .omitted
            .checked_add(remove as u64)
            .ok_or(StreamError::Capacity)?;
        for _ in 0..remove {
            self.items.pop_front();
        }
        self.items.push_back((value, charge));
        self.empty = empty;
        self.base = base;
        self.charged = charged + charge;
        self.omitted = omitted;
        Ok(())
    }
    pub fn omitted(&self) -> u64 {
        self.omitted
    }
    pub fn value(&self) -> Value {
        if self.items.is_empty() {
            return self.empty.clone();
        }
        let provenance =
            Provenance::agreed_by(self.items.iter().map(|(value, _)| value.provenance()))
                .inheriting(self.empty.provenance());
        Value::new(
            self.empty.shape().clone(),
            Data::List(
                self.items
                    .iter()
                    .map(|(value, _)| value.data().clone())
                    .collect(),
            ),
            provenance,
        )
        .expect("list data has list shape")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(n: i64, provenance: Provenance) -> Value {
        Value::new(Shape::Unknown, Data::Int(n), provenance).unwrap()
    }
    #[test]
    fn evicting_sensitive_items_does_not_erase_window_policy() {
        let mut window = Window::new(
            &Shape::Unknown,
            Provenance::default(),
            Limits {
                items: 1.try_into().unwrap(),
                ..Default::default()
            },
        )
        .unwrap();
        window
            .push(item(
                1,
                Provenance::default().with_policy(
                    &wes_core::flow::FlowPolicy::default()
                        .private()
                        .from_origin("prod"),
                ),
            ))
            .unwrap();
        window.push(item(2, Provenance::default())).unwrap();
        assert_eq!(window.omitted(), 1);
        assert!(window.value().provenance().policy().is_private());
        assert!(
            window
                .value()
                .provenance()
                .policy()
                .origins()
                .contains("prod")
        );
    }
    #[test]
    fn byte_budget_evicts_oldest_and_a_rejected_item_leaves_window_and_count_unchanged() {
        let one = item(1, Provenance::default());
        let mut window =
            Window::new(&Shape::Unknown, Provenance::default(), Limits::default()).unwrap();
        let charge = value_charge(&one, u64::MAX).unwrap();
        window.limits.bytes = (window.charged + charge * 2).try_into().unwrap();
        for n in 1..=3 {
            window.push(item(n, Provenance::default())).unwrap();
        }
        assert_eq!(
            window.value().data(),
            &Data::List(vec![Data::Int(2), Data::Int(3)])
        );
        assert_eq!(window.omitted(), 1);
        assert_eq!(window.charged, window.limits.bytes.get());
        let before = window.value();
        let too_big = Value::new(
            Shape::Unknown,
            Data::Bytes(vec![0; 5000].into()),
            Provenance::default(),
        )
        .unwrap();
        assert_eq!(window.push(too_big), Err(StreamError::Capacity));
        assert_eq!(window.value(), before);
        assert_eq!(window.omitted(), 1);
    }
    #[test]
    fn provenance_is_recomputed_after_eviction_and_attribution_survives_every_window() {
        let limits = Limits {
            items: 2.try_into().unwrap(),
            ..Default::default()
        };
        let base = Provenance::default()
            .with_fact("input", "captured")
            .cautioned(["unchecked:call".into()]);
        let mut window = Window::new(&Shape::Unknown, base.clone(), limits).unwrap();
        assert_eq!(window.value().provenance(), &base);
        window
            .push(item(
                1,
                Provenance::default()
                    .with_fact("source", "old")
                    .cautioned(["old-caution".into()]),
            ))
            .unwrap();
        window
            .push(item(2, Provenance::default().with_fact("source", "new")))
            .unwrap();
        assert_eq!(window.value().provenance().fact("source"), None);
        window
            .push(item(3, Provenance::default().with_fact("source", "new")))
            .unwrap();
        let value = window.value();
        assert_eq!(value.provenance().fact("source"), Some("new"));
        assert_eq!(value.provenance().fact("input"), Some("captured"));
        assert!(value.provenance().cautions().contains("unchecked:call"));
        assert!(!value.provenance().cautions().contains("old-caution"));
    }
    #[test]
    fn metadata_depth_and_omission_overflow_fail_without_replacing_old_values() {
        let mut shape = Shape::Unknown;
        for _ in 0..257 {
            shape = Shape::List(Box::new(shape));
        }
        assert!(Window::new(&shape, Provenance::default(), Limits::default()).is_err());
        let mut window = Window::new(
            &Shape::Unknown,
            Provenance::default(),
            Limits {
                items: 1.try_into().unwrap(),
                ..Default::default()
            },
        )
        .unwrap();
        window.push(item(1, Provenance::default())).unwrap();
        window.omitted = u64::MAX;
        assert_eq!(
            window.push(item(2, Provenance::default())),
            Err(StreamError::Capacity)
        );
        assert_eq!(window.value().data(), &Data::List(vec![Data::Int(1)]));
    }
}
