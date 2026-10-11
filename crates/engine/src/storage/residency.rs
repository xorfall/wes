//! Shared decoded allocation admission. Weak witnesses follow the actual payload owners.
use super::StoreError;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use wes_core::{Allocation, AllocationKey, Value};

#[derive(Clone)]
pub struct ResidentMemory(Arc<Mutex<State>>);
struct State {
    limit: u64,
    maximum: usize,
    allocations: BTreeMap<AllocationKey, Allocation>,
    reserved: u64,
}
impl std::fmt::Debug for ResidentMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResidentMemory")
            .field("used", &self.used())
            .finish()
    }
}
impl ResidentMemory {
    pub fn new(limit: u64, maximum: usize) -> Self {
        Self(Arc::new(Mutex::new(State {
            limit,
            maximum,
            allocations: BTreeMap::new(),
            reserved: 0,
        })))
    }
    pub fn used(&self) -> u64 {
        let mut s = self.0.lock().expect("resident memory");
        s.used()
    }
    pub fn limit(&self) -> u64 {
        self.0.lock().expect("resident memory").limit
    }
    pub fn admit_snapshot(&self, snapshot: &super::StoredSnapshot) -> Result<(), StoreError> {
        self.0
            .lock()
            .expect("resident memory")
            .admit_allocations(vec![snapshot.allocation()])
    }
    pub fn admit(&self, value: &Value) -> Result<(), StoreError> {
        let mut s = self.0.lock().expect("resident memory");
        s.admit(value)
    }

    /// Reserve a conservative decode bound before touching payload bytes. The reader must keep
    /// this reservation until registration or refusal, including an entered blocking operation.
    pub fn reserve(&self, bytes: u64) -> Result<Reservation, StoreError> {
        let mut s = self.0.lock().expect("resident memory");
        if s.used()
            .checked_add(bytes)
            .is_none_or(|used| used > s.limit)
        {
            return Err(StoreError::Limit("materialization memory"));
        }
        s.reserved += bytes;
        Ok(Reservation {
            memory: self.clone(),
            bytes,
        })
    }
}
impl State {
    fn admit(&mut self, value: &Value) -> Result<(), StoreError> {
        let allocations = value
            .allocations(self.maximum)
            .ok_or(StoreError::Limit("resident allocation metadata"))?;
        self.admit_allocations(allocations)
    }
    fn admit_allocations(&mut self, allocations: Vec<Allocation>) -> Result<(), StoreError> {
        // An immutable allocation already charged under a live witness needs no new
        // admission. Reaping the whole registry for every such value makes cache
        // maintenance quadratic and can starve publication progress under load.
        if allocations.iter().all(|allocation| {
            self.allocations
                .get(&allocation.key())
                .is_some_and(|known| known.alive() && known.bytes() == allocation.bytes())
        }) {
            return Ok(());
        }
        let used = self.used();
        let mut incoming = BTreeMap::new();
        for allocation in allocations {
            if !self.allocations.contains_key(&allocation.key()) {
                incoming.entry(allocation.key()).or_insert(allocation);
            }
        }
        let charge = incoming
            .values()
            .fold(0u64, |n, a| n.saturating_add(a.bytes().saturating_add(96)));
        if self.allocations.len().saturating_add(incoming.len()) > self.maximum
            || used.saturating_add(charge) > self.limit
        {
            return Err(StoreError::Limit("resident values"));
        }
        self.allocations.extend(incoming);
        Ok(())
    }
    fn used(&mut self) -> u64 {
        self.allocations.retain(|_, a| a.alive());
        self.reserved.saturating_add(
            self.allocations
                .values()
                .fold(0u64, |n, a| n.saturating_add(a.bytes().saturating_add(96))),
        )
    }
}
pub struct Reservation {
    memory: ResidentMemory,
    bytes: u64,
}
impl Reservation {
    pub fn finish(mut self, value: &Value) -> Result<(), StoreError> {
        let mut s = self.memory.0.lock().expect("resident memory");
        s.reserved -= self.bytes;
        self.bytes = 0;
        s.admit(value)
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.memory.0.lock().expect("resident memory").reserved -= self.bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wes_core::{Data, Primitive, Provenance, Shape};

    #[test]
    fn shared_readmission_preserves_full_reservations_and_last_owner_accounting() {
        let value = Value::new(
            Shape::Primitive(Primitive::Text),
            Data::Text("synthetic shared allocation".into()),
            Provenance::default(),
        )
        .unwrap();
        let charge = value
            .allocations(64)
            .unwrap()
            .iter()
            .map(|allocation| allocation.bytes() + 96)
            .sum::<u64>();
        let memory = ResidentMemory::new(charge + 1024, 64);
        memory.admit(&value).unwrap();
        let alias = value.clone();
        let reservation = memory.reserve(1024).unwrap();
        memory.admit(&alias).unwrap();
        assert_eq!(memory.used(), charge + 1024);

        let other = Value::new(
            Shape::Primitive(Primitive::Int),
            Data::Int(7),
            Provenance::default(),
        )
        .unwrap();
        assert!(matches!(memory.admit(&other), Err(StoreError::Limit(_))));
        drop(value);
        assert_eq!(memory.used(), charge + 1024);
        drop(alias);
        assert_eq!(memory.used(), 1024);
        drop(reservation);
        assert_eq!(memory.used(), 0);
        memory.admit(&other).unwrap();
        assert!(memory.used() > 0);
        drop(other);
        assert_eq!(memory.used(), 0);
    }
}
