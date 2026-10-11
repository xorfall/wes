//! Process-owned storage scopes. An identity persisted on disk never renews this lifetime.
use super::{StoreError, ValueHandle, ValueLease};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatasetLifetime {
    #[default]
    Durable,
    Transient(String),
}
impl DatasetLifetime {
    pub fn valid(&self) -> bool {
        match self {
            Self::Durable => true,
            Self::Transient(id) => ValueHandle::new(id).is_ok(),
        }
    }
}
pub(crate) struct State {
    id: String,
    open: AtomicBool,
    captures: Mutex<BTreeMap<ValueHandle, ValueLease>>,
}
#[derive(Clone)]
pub struct TransientScope(Arc<State>);
#[derive(Clone)]
pub struct TransientScopeWitness(Weak<State>);
impl std::fmt::Debug for TransientScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransientScope")
            .field("id", &self.id())
            .field("open", &self.is_open())
            .finish()
    }
}
impl TransientScope {
    pub(crate) fn new() -> Self {
        Self(Arc::new(State {
            id: uuid::Uuid::new_v4().to_string(),
            open: AtomicBool::new(true),
            captures: Mutex::new(BTreeMap::new()),
        }))
    }
    pub fn id(&self) -> &str {
        &self.0.id
    }
    pub fn is_open(&self) -> bool {
        self.0.open.load(Ordering::Acquire)
    }
    pub fn witness(&self) -> TransientScopeWitness {
        TransientScopeWitness(Arc::downgrade(&self.0))
    }
    pub(crate) fn capture(&self, handle: ValueHandle, lease: ValueLease) -> Result<(), StoreError> {
        let mut captures = self.0.captures.lock().map_err(|_| StoreError::Closed)?;
        if !self.is_open() {
            return Err(StoreError::Closed);
        }
        if !captures.contains_key(&handle)
            && captures.len() >= wes_budgets::get("values.outputs") as usize
        {
            return Err(StoreError::Limit("transient captures"));
        }
        captures.insert(handle, lease);
        Ok(())
    }
    pub(crate) fn close(&self) {
        self.0.open.store(false, Ordering::Release);
        if let Ok(mut captures) = self.0.captures.lock() {
            captures.clear();
        }
    }
}
impl TransientScopeWitness {
    pub fn is_open(&self) -> bool {
        self.0
            .upgrade()
            .is_some_and(|s| s.open.load(Ordering::Acquire))
    }
}
