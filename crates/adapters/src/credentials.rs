//! Non-persistent credentials with explicit runtime overrides and named environment fallback.
use indexmap::IndexMap;
use std::{
    fmt,
    sync::{Arc, RwLock},
};
use wes_engine::credentials::{CredentialError, Credentials, ExposeSecret, Secret, SecretString};

#[derive(Clone, Copy, Debug)]
pub struct CredentialLimits {
    pub entries: usize,
    pub name_bytes: usize,
    pub value_bytes: usize,
    /// Sum of supplied names and values; not allocator overhead or caller/in-flight snapshots.
    pub total_bytes: usize,
}
impl Default for CredentialLimits {
    fn default() -> Self {
        Self {
            entries: wes_budgets::get("credentials.entries") as usize,
            name_bytes: 256,
            value_bytes: wes_budgets::get("credentials.value.bytes") as usize,
            total_bytes: wes_budgets::get("credentials.total.bytes") as usize,
        }
    }
}
type Environment = dyn Fn(&str) -> Result<Option<SecretString>, CredentialError> + Send + Sync;
#[derive(Default)]
struct Supplied {
    entries: IndexMap<String, Secret>,
    bytes: usize,
}
pub struct MemoryCredentials {
    supplied: RwLock<Supplied>,
    environment: Arc<Environment>,
    limits: CredentialLimits,
}
impl MemoryCredentials {
    pub fn new(limits: CredentialLimits) -> Self {
        Self::with_environment(limits, |variable| match std::env::var(variable) {
            Ok(value) => Ok(Some(SecretString::from(value))),
            Err(std::env::VarError::NotPresent) => Ok(None),
            // Do not retain VarError: its NotUnicode payload contains the credential itself.
            Err(std::env::VarError::NotUnicode(_)) => Err(CredentialError::InvalidEnvironment),
        })
    }
    /// Injection keeps tests/configuration independent of global process-environment mutation.
    /// The callback sees only the requested variable name, never a whole environment snapshot.
    pub fn with_environment(
        limits: CredentialLimits,
        environment: impl Fn(&str) -> Result<Option<SecretString>, CredentialError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            supplied: RwLock::default(),
            environment: Arc::new(environment),
            limits,
        }
    }
    /// Whitespace-only input removes the runtime override, revealing any environment fallback.
    /// Validation and capacity checks finish before mutation. This never writes to disk.
    pub fn remember(&self, name: String, value: SecretString) -> Result<(), CredentialError> {
        self.validate_name(&name)?;
        let text = value.expose_secret();
        if text.len() > self.limits.value_bytes {
            return Err(CredentialError::TooLarge);
        }
        let blank = is_blank(text);
        let mut supplied = self
            .supplied
            .write()
            .map_err(|_| CredentialError::Unavailable)?;
        let previous = supplied
            .entries
            .get(&name)
            .map_or(0, |value| name.len() + value.expose_secret().len());
        let added = if blank {
            0
        } else {
            name.len()
                .checked_add(text.len())
                .ok_or(CredentialError::Capacity)?
        };
        let total = (supplied.bytes - previous)
            .checked_add(added)
            .ok_or(CredentialError::Capacity)?;
        if total > self.limits.total_bytes
            || (!blank
                && !supplied.entries.contains_key(&name)
                && supplied.entries.len() >= self.limits.entries)
        {
            return Err(CredentialError::Capacity);
        }
        if blank {
            supplied.entries.shift_remove(&name);
        } else {
            supplied.entries.insert(name, Arc::new(value));
        }
        supplied.bytes = total;
        Ok(())
    }
    /// Only names explicitly supplied during this process. Environment variables are not enumerated.
    pub fn names(&self) -> Result<Vec<String>, CredentialError> {
        Ok(self
            .supplied
            .read()
            .map_err(|_| CredentialError::Unavailable)?
            .entries
            .keys()
            .cloned()
            .collect())
    }
    fn validate_name(&self, name: &str) -> Result<(), CredentialError> {
        if is_blank(name)
            || name.len() > self.limits.name_bytes
            || name.chars().any(|ch| ch.is_control() || ch == '=')
        {
            return Err(CredentialError::InvalidName);
        }
        Ok(())
    }
}
impl Credentials for MemoryCredentials {
    fn lookup(&self, name: &str) -> Result<Option<Secret>, CredentialError> {
        self.validate_name(name)?;
        if let Some(value) = self
            .supplied
            .read()
            .map_err(|_| CredentialError::Unavailable)?
            .entries
            .get(name)
            .cloned()
        {
            return Ok(Some(value));
        }
        // The store lock is released before calling external configuration code.
        let Some(value) = (self.environment)(&environment_variable(name))? else {
            return Ok(None);
        };
        if value.expose_secret().len() > self.limits.value_bytes {
            return Err(CredentialError::TooLarge);
        }
        if is_blank(value.expose_secret()) {
            return Ok(None);
        }
        Ok(Some(Arc::new(value)))
    }
}
impl fmt::Debug for MemoryCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryCredentials")
            .field("supplied_names", &self.names())
            .finish_non_exhaustive()
    }
}
pub fn environment_variable(name: &str) -> String {
    format!("WES_{}", name.to_uppercase().replace(['-', '.'], "_"))
}
fn is_blank(value: &str) -> bool {
    // Non-breaking spaces are not blank tokens in the credential language.
    value.chars().all(|ch| matches!(ch, '\u{9}'..='\u{d}' | '\u{1c}'..='\u{20}' | '\u{1680}' |
        '\u{2000}'..='\u{2006}' | '\u{2008}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{205f}' | '\u{3000}'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{OnceLock, Weak};

    #[test]
    fn external_environment_callback_is_never_called_under_the_store_lock() {
        let owner = Arc::new(OnceLock::<Weak<MemoryCredentials>>::new());
        let captured = owner.clone();
        let store = Arc::new(MemoryCredentials::with_environment(
            CredentialLimits::default(),
            move |_| {
                let store = captured.get().unwrap().upgrade().unwrap();
                assert!(store.supplied.try_write().is_ok());
                Ok(None)
            },
        ));
        owner.set(Arc::downgrade(&store)).unwrap();
        assert!(store.lookup("fixture").unwrap().is_none());
    }

    #[test]
    fn a_poisoned_store_returns_a_generic_failure_without_exposing_contents() {
        let store = Arc::new(MemoryCredentials::with_environment(
            CredentialLimits::default(),
            |_| Ok(None),
        ));
        store
            .remember(
                "key".into(),
                SecretString::from("fixture-private".to_owned()),
            )
            .unwrap();
        let shared = store.clone();
        assert!(
            std::thread::spawn(move || {
                let _guard = shared.supplied.write().unwrap();
                panic!("injected store poison");
            })
            .join()
            .is_err()
        );
        assert!(matches!(
            store.lookup("key"),
            Err(CredentialError::Unavailable)
        ));
        assert_eq!(store.names(), Err(CredentialError::Unavailable));
        assert!(!format!("{store:?}").contains("fixture-private"));
    }
}
