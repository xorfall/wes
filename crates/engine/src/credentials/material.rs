//! Material is independent of execution grants. Hydrate/supply/forget run on an
//! owned blocking worker; invocation lookup reads only the bounded memory cache.
use super::{CredentialError, ExposeSecret, Secret};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};
pub trait SecureStore: Send + Sync {
    fn supported(&self) -> bool;
    /// Changes whenever previously read values or absences may no longer hold, for example when
    /// a vault is locked, unlocked or reset. Reading it must not perform I/O.
    fn generation(&self) -> u64 {
        0
    }
    fn get(&self, reference: &str) -> Result<Option<Secret>, CredentialError>;
    fn set(&self, reference: &str, value: &Secret) -> Result<(), CredentialError>;
    fn remove(&self, reference: &str) -> Result<(), CredentialError>;
}
#[derive(Default)]
struct State {
    values: BTreeMap<String, Secret>,
    saved: BTreeSet<String>,
    loaded: BTreeSet<String>,
    bytes: usize,
    generation: u64,
}
#[derive(Clone, Default)]
pub struct Material {
    state: Arc<Mutex<State>>,
    operation: Arc<Mutex<()>>,
    secure: Option<Arc<dyn SecureStore>>,
}
impl Material {
    pub fn new(secure: Arc<dyn SecureStore>) -> Self {
        Self {
            secure: Some(secure),
            ..Self::default()
        }
    }
    pub fn supported(&self) -> bool {
        self.secure.as_ref().is_some_and(|s| s.supported())
    }
    fn generation(&self) -> u64 {
        self.secure.as_ref().map_or(0, |s| s.generation())
    }
    /// The bounded cache with remembered material dropped once the secure store changed state.
    /// Session-only values never came from the store and remain available.
    fn state(&self) -> Result<std::sync::MutexGuard<'_, State>, CredentialError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| CredentialError::Unavailable)?;
        let generation = self.generation();
        if state.generation != generation {
            for reference in std::mem::take(&mut state.saved) {
                if let Some(value) = state.values.remove(&reference) {
                    state.bytes -= reference.len() + value.expose_secret().len();
                }
            }
            let State { values, loaded, .. } = &mut *state;
            loaded.retain(|reference| values.contains_key(reference));
            state.generation = generation;
        }
        Ok(state)
    }
    fn validate(reference: &str) -> Result<(), CredentialError> {
        if reference.is_empty() || reference.len() > 256 || reference.chars().any(char::is_control)
        {
            Err(CredentialError::InvalidName)
        } else {
            Ok(())
        }
    }
    fn charge(state: &State, reference: &str, value: &Secret) -> Result<usize, CredentialError> {
        let size = value.expose_secret().len();
        if size == 0 {
            return Err(CredentialError::Empty);
        }
        if size > 64 * 1024 {
            return Err(CredentialError::TooLarge);
        }
        let old = state
            .values
            .get(reference)
            .map_or(0, |s| reference.len() + s.expose_secret().len());
        let bytes = state.bytes - old + reference.len() + size;
        if bytes > 1024 * 1024 || (!state.loaded.contains(reference) && state.loaded.len() >= 1024)
        {
            return Err(CredentialError::Capacity);
        }
        Ok(bytes)
    }
    pub fn hydrate(&self, reference: &str) -> Result<(), CredentialError> {
        Self::validate(reference)?;
        let _operation = self
            .operation
            .lock()
            .map_err(|_| CredentialError::Unavailable)?;
        let state = self.state()?;
        if state.loaded.contains(reference) {
            return Ok(());
        }
        if state.loaded.len() >= 1024 {
            return Err(CredentialError::Capacity);
        }
        let generation = state.generation;
        drop(state);
        let loaded = self
            .secure
            .as_ref()
            .map(|store| store.get(reference))
            .transpose()?
            .flatten();
        let mut state = self.state()?;
        if state.generation != generation {
            // The store changed state during the read; a later hydration reads it again.
            return Ok(());
        }
        if let Some(value) = loaded {
            state.bytes = Self::charge(&state, reference, &value)?;
            state.values.insert(reference.into(), value);
            state.saved.insert(reference.into());
        }
        state.loaded.insert(reference.into());
        Ok(())
    }
    pub fn supply(
        &self,
        reference: String,
        value: Secret,
        remember: bool,
    ) -> Result<(), CredentialError> {
        Self::validate(&reference)?;
        let _operation = self
            .operation
            .lock()
            .map_err(|_| CredentialError::Unavailable)?;
        let state = self.state()?;
        Self::charge(&state, &reference, &value)?;
        let generation = state.generation;
        drop(state);
        if remember {
            self.secure
                .as_ref()
                .filter(|s| s.supported())
                .ok_or(CredentialError::Unavailable)?
                .set(&reference, &value)?;
        } else if let Some(store) = &self.secure {
            // Session-only replacement must not leave an old saved value to reappear.
            store.remove(&reference)?;
        }
        let mut state = self.state()?;
        if remember && state.generation != generation {
            // The store was locked or reset after accepting the value; it hydrates from the store
            // again once available instead of outliving the lock in memory.
            return Ok(());
        }
        state.bytes = Self::charge(&state, &reference, &value)?;
        state.values.insert(reference.clone(), value);
        state.loaded.insert(reference.clone());
        if remember {
            state.saved.insert(reference);
        } else {
            state.saved.remove(&reference);
        }
        Ok(())
    }
    pub fn forget(&self, reference: &str) -> Result<(), CredentialError> {
        Self::validate(reference)?;
        let _operation = self
            .operation
            .lock()
            .map_err(|_| CredentialError::Unavailable)?;
        let state = self.state()?;
        drop(state);
        if let Some(store) = &self.secure {
            store.remove(reference)?;
        }
        let mut state = self.state()?;
        if let Some(value) = state.values.remove(reference) {
            state.bytes -= reference.len() + value.expose_secret().len();
        }
        state.saved.remove(reference);
        state.loaded.remove(reference);
        Ok(())
    }
    pub fn lookup(&self, reference: &str) -> Result<Option<Secret>, CredentialError> {
        Ok(self.state()?.values.get(reference).cloned())
    }
    pub fn status(&self, reference: &str) -> Result<(bool, bool), CredentialError> {
        let state = self.state()?;
        Ok((
            state.values.contains_key(reference),
            state.saved.contains(reference),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    #[derive(Default)]
    struct Store {
        values: Mutex<BTreeMap<String, Secret>>,
        fail: AtomicBool,
    }
    impl SecureStore for Store {
        fn supported(&self) -> bool {
            true
        }
        fn get(&self, r: &str) -> Result<Option<Secret>, CredentialError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(CredentialError::Unavailable);
            }
            Ok(self.values.lock().unwrap().get(r).cloned())
        }
        fn set(&self, r: &str, v: &Secret) -> Result<(), CredentialError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(CredentialError::Unavailable);
            }
            self.values.lock().unwrap().insert(r.into(), v.clone());
            Ok(())
        }
        fn remove(&self, r: &str) -> Result<(), CredentialError> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(CredentialError::Unavailable);
            }
            self.values.lock().unwrap().remove(r);
            Ok(())
        }
    }
    #[derive(Default)]
    struct Vault {
        values: Mutex<BTreeMap<String, Secret>>,
        locked: AtomicBool,
        generation: std::sync::atomic::AtomicU64,
    }
    impl Vault {
        fn set_locked(&self, locked: bool) {
            self.locked.store(locked, Ordering::SeqCst);
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
        fn open(&self) -> Result<(), CredentialError> {
            if self.locked.load(Ordering::SeqCst) {
                Err(CredentialError::Locked)
            } else {
                Ok(())
            }
        }
    }
    impl SecureStore for Vault {
        fn supported(&self) -> bool {
            true
        }
        fn generation(&self) -> u64 {
            self.generation.load(Ordering::SeqCst)
        }
        fn get(&self, r: &str) -> Result<Option<Secret>, CredentialError> {
            self.open()?;
            Ok(self.values.lock().unwrap().get(r).cloned())
        }
        fn set(&self, r: &str, v: &Secret) -> Result<(), CredentialError> {
            self.open()?;
            self.values.lock().unwrap().insert(r.into(), v.clone());
            Ok(())
        }
        fn remove(&self, r: &str) -> Result<(), CredentialError> {
            self.open()?;
            self.values.lock().unwrap().remove(r);
            Ok(())
        }
    }
    #[test]
    fn should_drop_remembered_material_and_reread_absences_when_the_store_changes_state() {
        // Arrange
        let vault = Arc::new(Vault::default());
        let m = Material::new(vault.clone());
        m.supply("saved".into(), secret("remembered"), true)
            .unwrap();
        m.supply("session".into(), secret("transient"), false)
            .unwrap();
        m.hydrate("absent").unwrap();

        // Act
        vault.set_locked(true);

        // Assert
        assert!(m.lookup("saved").unwrap().is_none());
        assert_eq!(m.status("saved").unwrap(), (false, false));
        assert_eq!(
            m.lookup("session").unwrap().unwrap().expose_secret(),
            "transient"
        );
        assert_eq!(m.hydrate("saved"), Err(CredentialError::Locked));
        assert_eq!(
            m.supply("other".into(), secret("value"), true),
            Err(CredentialError::Locked)
        );
        vault
            .values
            .lock()
            .unwrap()
            .insert("absent".into(), secret("added while locked"));
        vault.set_locked(false);
        m.hydrate("saved").unwrap();
        m.hydrate("absent").unwrap();
        assert_eq!(
            m.lookup("saved").unwrap().unwrap().expose_secret(),
            "remembered"
        );
        assert_eq!(m.status("saved").unwrap(), (true, true));
        assert_eq!(
            m.lookup("absent").unwrap().unwrap().expose_secret(),
            "added while locked"
        );
    }
    fn secret(s: &str) -> Secret {
        Arc::new(super::super::SecretString::from(s))
    }
    #[test]
    fn saved_material_shares_by_reference_and_session_replacement_removes_the_old_saved_value() {
        let store = Arc::new(Store::default());
        let m = Material::new(store.clone());
        let peer = m.clone();
        m.supply("a".into(), secret("first"), true).unwrap();
        assert_eq!(peer.status("a").unwrap(), (true, true));
        let restart = Material::new(store.clone());
        restart.hydrate("a").unwrap();
        assert_eq!(
            restart.lookup("a").unwrap().unwrap().expose_secret(),
            "first"
        );
        m.supply("a".into(), secret("replacement"), false).unwrap();
        assert_eq!(peer.status("a").unwrap(), (true, false));
        let restart = Material::new(store);
        restart.hydrate("a").unwrap();
        assert!(restart.lookup("a").unwrap().is_none());
        m.forget("a").unwrap();
        assert_eq!(peer.status("a").unwrap(), (false, false));
    }
    #[test]
    fn failed_secure_writes_never_claim_persistence_or_silently_reappear_after_forget() {
        let store = Arc::new(Store::default());
        let m = Material::new(store.clone());
        m.supply("a".into(), secret("old"), true).unwrap();
        store.fail.store(true, Ordering::SeqCst);
        assert!(m.supply("a".into(), secret("new"), false).is_err());
        assert!(m.forget("a").is_err());
        assert_eq!(m.lookup("a").unwrap().unwrap().expose_secret(), "old");
        assert!(Material::new(store.clone()).hydrate("a").is_err());
        store.fail.store(false, Ordering::SeqCst);
        m.forget("a").unwrap();
        assert_eq!(m.status("a").unwrap(), (false, false));
        assert!(
            Material::default()
                .supply("a".into(), secret("v"), true)
                .is_err()
        );
    }
}

#[cfg(test)]
mod empty_tests {
    use super::*;
    #[test]
    fn empty_supply_rejects_without_replacing_existing_material_or_echoing_it() {
        let material = Material::default();
        material
            .supply(
                "fixture/key".into(),
                Arc::new(super::super::SecretString::from("synthetic-existing")),
                false,
            )
            .unwrap();
        let error = material
            .supply(
                "fixture/key".into(),
                Arc::new(super::super::SecretString::from("")),
                false,
            )
            .unwrap_err();
        assert_eq!(error, CredentialError::Empty);
        assert!(error.to_string().contains("nonempty"));
        assert!(!error.to_string().contains("synthetic-existing"));
        assert_eq!(
            material
                .lookup("fixture/key")
                .unwrap()
                .unwrap()
                .expose_secret(),
            "synthetic-existing"
        );
    }
}
