//! Provider read dependencies share resident, cold, held and Dataset gates.
//! Registration is a trusted composition operation; witnesses never register a
//! reader, grant permission, invoke a provider, or reconstruct an old connection.
use indexmap::IndexMap;
use sha2::{Digest, Sha256};
use std::{
    fmt,
    sync::{Arc, RwLock},
};
use wes_core::{
    Data, Primitive, Shape, Value,
    capability::ProviderDescription,
    flow::{FlowPolicy, ProviderReadOrigin},
};

/// Inert opt-in declaration. The finite Bool probe must return true only after
/// authenticating/authorizing the exact captured scope. It is always explicit.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadDomain {
    pub scope: Vec<String>,
    pub revalidate: Vec<String>,
}
impl ReadDomain {
    pub fn validate(&self, description: &ProviderDescription) -> Result<(), &'static str> {
        if self.scope.is_empty()
            || self.scope.len() > 16
            || self
                .scope
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.scope.len()
        {
            return Err("provider read scope requires 1–16 distinct required Text parameters");
        }
        let mut probe = false;
        for cap in description.capabilities() {
            for name in &self.scope {
                if !cap.parameters.iter().any(|p| {
                    &p.name == name && p.required && p.shape == Shape::Primitive(Primitive::Text)
                }) {
                    return Err("every read-domain operation must declare its required Text scope");
                }
            }
            if cap.path == self.revalidate {
                if cap.streaming
                    || cap.result != Shape::Primitive(Primitive::Bool)
                    || cap.parameters.len() != self.scope.len()
                {
                    return Err(
                        "read revalidation must be one finite Bool operation with only scope parameters",
                    );
                }
                probe = true;
            }
        }
        if !probe {
            return Err("provider read revalidation operation is missing");
        }
        Ok(())
    }
    pub(crate) fn digest(
        &self,
        arguments: &IndexMap<String, Value>,
    ) -> Result<String, &'static str> {
        let mut scope = self.scope.iter().collect::<Vec<_>>();
        scope.sort();
        let mut hash = Sha256::new();
        hash.update(b"wes.provider-read-scope/v1");
        for name in scope {
            let Some(value) = arguments.get(name) else {
                return Err("provider read scope is missing");
            };
            let Data::Text(text) = value.data() else {
                return Err("provider read scope is not Text");
            };
            if text.len() > 1024 {
                return Err("provider read scope exceeds its byte bound");
            }
            for s in [name.as_str(), text.as_ref()] {
                hash.update((s.len() as u64).to_be_bytes());
                hash.update(s.as_bytes());
            }
        }
        Ok(format!("{:x}", hash.finalize()))
    }
}

/// A process-local, no-I/O read owner. Ports contain live authority; never put
/// these handles in a Value or infer them from a realm/digest.
pub trait ReadAuthority: Send + Sync + 'static {
    fn identity(&self) -> String;
    fn realm(&self) -> Option<String>;
    fn closed(&self) -> bool;
    fn revision(&self) -> String;
    fn allows(&self, origin: &ProviderReadOrigin) -> bool;
    fn observe(&self, _observer: std::sync::Weak<Changes>) -> Result<(), &'static str> {
        Ok(())
    }
    fn deadline(&self) -> Option<std::time::Instant> {
        None
    }
    fn expire(&self) {}
}
pub use crate::credentials::material::ChangeSignal as Changes;
#[derive(Clone)]
pub(crate) struct ReadGuard(Arc<dyn Fn() -> bool + Send + Sync>);
impl ReadGuard {
    pub(crate) fn new(check: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(check))
    }
    pub(crate) fn current(&self) -> bool {
        (self.0)()
    }
}
#[derive(Clone)]
pub struct Readers(Arc<RwLock<Vec<Arc<dyn ReadAuthority>>>>, Arc<Changes>);
impl Default for Readers {
    fn default() -> Self {
        Self(Arc::default(), Arc::new(Changes::default()))
    }
}
impl fmt::Debug for Readers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProviderReaders(..)")
    }
}
impl PartialEq for Readers {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for Readers {}
impl Readers {
    pub fn register(&self, reader: Arc<dyn ReadAuthority>) -> Result<(), &'static str> {
        let mut readers = self
            .0
            .write()
            .map_err(|_| "provider read owner is unavailable")?;
        readers.retain(|p| !p.closed());
        if readers.iter().any(|p| p.identity() == reader.identity()) {
            return Ok(());
        }
        if readers.len() >= wes_budgets::get("provider.read.owners") as usize {
            return Err("provider read owner capacity exceeded");
        }
        reader.observe(Arc::downgrade(&self.1))?;
        readers.push(reader);
        self.1.changed();
        Ok(())
    }
    pub fn changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.1.subscribe()
    }
    pub fn deadline(&self) -> Option<std::time::Instant> {
        self.0
            .read()
            .ok()?
            .iter()
            .filter_map(|p| p.deadline())
            .min()
    }
    pub fn expire(&self) {
        if let Ok(readers) = self.0.read() {
            for p in readers.iter() {
                p.expire();
            }
        }
    }
    pub fn blocks_policy(&self, policy: &FlowPolicy) -> bool {
        if policy.provider_reads().is_empty() {
            return false;
        }
        let Ok(readers) = self.0.read() else {
            return true;
        };
        policy.is_unknown()
            || policy.provider_reads().iter().any(|origin| {
                let mut matches = readers
                    .iter()
                    .filter(|p| p.realm().as_deref() == Some(origin.realm()));
                let Some(reader) = matches.next() else {
                    return true;
                };
                matches.next().is_some() || !reader.allows(origin)
            })
    }
    /// Payload-free epoch for admission and late reply revalidation. Including
    /// denied realms ensures revoke/regrant cannot resurrect an in-flight read.
    pub fn epoch(&self) -> String {
        let mut hash = Sha256::new();
        hash.update(b"wes.provider-read-realm/v1");
        match self.0.read() {
            Err(_) => hash.update(b"unavailable"),
            Ok(readers) => {
                for p in readers.iter() {
                    for s in [p.identity(), p.realm().unwrap_or_default(), p.revision()] {
                        hash.update((s.len() as u64).to_be_bytes());
                        hash.update(s.as_bytes());
                    }
                }
            }
        }
        format!("{:x}", hash.finalize())
    }
}
