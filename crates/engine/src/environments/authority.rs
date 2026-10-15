//! Explicit session-lifetime material and grants. Never serialized or restored, never ambient.
use crate::{
    credentials::{CredentialError, Credentials, Secret},
    driver::CancellationToken,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use wes_core::environments::{Binding, Revision};
mod access;
use access::alive;
pub use access::{AccessEnd, AccessLifetime, AccessStatus};
mod provider_reads;
mod resources;
pub(crate) use provider_reads::ReadPermit;
pub use provider_reads::ReadProbe;
pub use resources::{InvocationAuthority, ResourceCreation};

/// Payload-independent refusal reasons from the same locked admission check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchDenied {
    Closed,
    Disabled,
    Restored,
    Unavailable,
    Transfer,
}
impl DispatchDenied {
    pub fn code(self) -> &'static str {
        "ENV020"
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::Closed => {
                "Environment execution authority has closed; select the environment in a new live session."
            }
            Self::Disabled => {
                "Environment is disabled for this live session; explicitly enable or select it again."
            }
            Self::Restored => {
                "Environment execution is closed after reopening; explicitly select it again or use --env NAME --env-revision REVISION --activate-env."
            }
            Self::Unavailable => {
                "Captured environment execution authority is unavailable; no provider was called."
            }
            Self::Transfer => {
                "Input provenance requires an explicit transfer grant to this destination; no provider was called."
            }
        }
    }
}
type Scope = (String, Revision, String);
struct Grant {
    identity: Arc<()>,
    until: Option<Instant>,
    lifetime: AccessLifetime,
    material_generation: u64,
    references: BTreeSet<String>,
    secrets: BTreeMap<String, Option<std::sync::Weak<crate::credentials::SecretString>>>,
}
impl Grant {
    fn current(&self, material: &crate::credentials::material::Material) -> bool {
        alive(self.until)
            && material
                .revision()
                .is_ok_and(|(generation, _)| generation == self.material_generation)
            && self.secrets.iter().all(|(r, before)| {
                material.lookup(r).is_ok_and(|now| match (before, now) {
                    (Some(a), Some(b)) => a.ptr_eq(&Arc::downgrade(&b)),
                    (None, None) => true,
                    _ => false,
                })
            })
    }
}
#[derive(Default)]
struct State {
    read_epoch: u64,
    read_observers: Vec<std::sync::Weak<crate::provider_reads::Changes>>,
    read_updates: Option<tokio::sync::watch::Sender<u64>>,
    reads: BTreeMap<wes_core::flow::ProviderReadOrigin, provider_reads::ReadGrant>,
    read_probes: BTreeMap<wes_core::flow::ProviderReadOrigin, std::sync::Weak<()>>,
    known_scopes: BTreeMap<wes_core::flow::ProviderReadOrigin, provider_reads::KnownScope>,
    resources: resources::Resources,
    restored: bool,
    lifetimes: BTreeMap<String, CancellationToken>,
    enabled: BTreeSet<String>,
    namespace: Option<String>,
    transfers: BTreeMap<(String, String), Instant>,
    grants: BTreeMap<Scope, Grant>,
    ended: BTreeMap<Scope, AccessEnd>,
    disabled: BTreeSet<String>,
    closed: bool,
}
#[derive(Clone, Default)]
pub struct Authority(Arc<RwLock<State>>, crate::credentials::material::Material);
impl fmt::Debug for Authority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EnvironmentAuthority(..)")
    }
}
#[derive(Clone, Debug, Default)]
pub struct CredentialStatus {
    pub present: BTreeSet<String>,
    pub saved: BTreeSet<String>,
    pub persistence_supported: bool,
    pub grant_seconds: u64,
    pub access: AccessStatus,
    pub read_scopes: Vec<provider_reads::ReadScopeStatus>,
    pub read_domain: Option<crate::provider_reads::ReadDomain>,
    pub enabled: bool,
}
impl Authority {
    pub fn with_material(material: crate::credentials::material::Material) -> Self {
        Self(Arc::default(), material)
    }
    /// Explicit secure-store I/O; run on an owned blocking worker, never an invoker.
    pub fn hydrate(&self, binding: &Binding) -> Result<(), CredentialError> {
        for reference in binding.import().credential_refs().values() {
            self.1.hydrate(reference)?;
        }
        Ok(())
    }

    /// Presence and remaining authority only; never clones or exposes secret material.
    pub fn status(
        &self,
        binding: &Binding,
        alias: &str,
    ) -> Result<CredentialStatus, CredentialError> {
        let scope = scope(binding, alias)?;
        let state = self.0.read().map_err(|_| CredentialError::Unavailable)?;
        let enabled = state.available(&scope.0);
        let mut present = BTreeSet::new();
        let mut saved = BTreeSet::new();
        for (slot, reference) in binding.import().credential_refs() {
            let (available, remembered) = self.1.status(reference)?;
            if available {
                present.insert(slot.clone());
            }
            if remembered {
                saved.insert(slot.clone());
            }
        }
        Ok(CredentialStatus {
            present,
            saved,
            persistence_supported: self.1.supported(),
            grant_seconds: if enabled {
                state
                    .grants
                    .get(&scope)
                    .and_then(|g| g.until)
                    .map(|at| at.saturating_duration_since(Instant::now()).as_secs())
                    .unwrap_or(0)
            } else {
                0
            },
            access: match state.grants.get(&scope).filter(|_| enabled) {
                Some(g) if g.current(&self.1) => match g.lifetime {
                    AccessLifetime::Timed { seconds } => AccessStatus::Timed { seconds },
                    AccessLifetime::Session(_) => AccessStatus::Session,
                },
                Some(g) => AccessStatus::Ended {
                    ended: if alive(g.until) {
                        AccessEnd::Changed
                    } else {
                        AccessEnd::Expired
                    },
                },
                None => state
                    .ended
                    .get(&scope)
                    .map(|reason| AccessStatus::Ended { ended: *reason })
                    .unwrap_or_default(),
            },
            read_scopes: self.read_scope_status(&state, &scope),
            read_domain: None,
            enabled,
        })
    }

    /// History contains definitions, never permission to resume execution.
    pub(crate) fn restored(&self) {
        if let Ok(mut state) = self.0.write() {
            state.restored = true;
            state.resources = Default::default();
            state.grants.clear();
            state.ended.clear();
            state.cancel_lifetimes();
            state.reads.clear();
            state.known_scopes.clear();
            state.read_changed();
        }
    }
    pub(crate) fn establish_namespace(&self, id: &str) {
        if let Ok(mut state) = self.0.write()
            && state.namespace.is_none()
        {
            state.namespace = Some(id.into());
            state.read_changed();
        }
    }
    pub fn origin(&self, environment: &str) -> Option<String> {
        self.0
            .read()
            .ok()?
            .namespace
            .as_ref()
            .map(|id| format!("{id}/{environment}"))
    }
    pub fn transfer(
        &self,
        origin: String,
        destination: String,
        lifetime: Duration,
    ) -> Result<(), CredentialError> {
        if origin.is_empty()
            || origin.len() > 256
            || destination.is_empty()
            || destination.len() > 256
            || lifetime.is_zero()
            || lifetime > Duration::from_secs(3600)
        {
            return Err(CredentialError::InvalidName);
        }
        let mut state = self.0.write().map_err(|_| CredentialError::Unavailable)?;
        if state.closed {
            return Err(CredentialError::Unavailable);
        }
        state.transfers.retain(|_, until| *until > Instant::now());
        if state.transfers.len() >= 1024 {
            return Err(CredentialError::Capacity);
        }
        state
            .transfers
            .insert((origin, destination), Instant::now() + lifetime);
        Ok(())
    }
    pub fn require_dispatch(
        &self,
        name: &str,
        policy: &wes_core::flow::FlowPolicy,
        revision: Revision,
    ) -> Result<(), DispatchDenied> {
        let state = self.0.read().map_err(|_| DispatchDenied::Unavailable)?;
        if state.closed {
            return Err(DispatchDenied::Closed);
        }
        if state.disabled.contains(name) {
            return Err(DispatchDenied::Disabled);
        }
        if state.restored && !state.enabled.contains(name) {
            return Err(DispatchDenied::Restored);
        }
        let namespace = state
            .namespace
            .as_ref()
            .ok_or(DispatchDenied::Unavailable)?;
        if state.permits(policy, &format!("{namespace}/{name}"), revision) {
            Ok(())
        } else {
            Err(DispatchDenied::Transfer)
        }
    }
    pub fn permits(
        &self,
        policy: &wes_core::flow::FlowPolicy,
        destination: &str,
        revision: Revision,
    ) -> bool {
        self.0
            .read()
            .is_ok_and(|state| !state.closed && state.permits(policy, destination, revision))
    }

    pub fn supply(&self, name: String, secret: Secret) -> Result<(), CredentialError> {
        self.supply_with_persistence(name, secret, false)
    }
    pub fn supply_with_persistence(
        &self,
        name: String,
        secret: Secret,
        remember: bool,
    ) -> Result<(), CredentialError> {
        let state = self.0.read().map_err(|_| CredentialError::Unavailable)?;
        if state.closed {
            return Err(CredentialError::Unavailable);
        }
        drop(state);
        let result = self.1.supply(name.clone(), secret, remember);
        self.invalidate_read_material(&name);
        result
    }
    pub fn forget(&self, name: &str) -> Result<(), CredentialError> {
        let result = self.1.forget(name);
        self.invalidate_read_material(name);
        result
    }
    pub fn grant(
        &self,
        binding: &Binding,
        alias: &str,
        lifetime: AccessLifetime,
    ) -> Result<(), CredentialError> {
        let until = lifetime.deadline()?;
        let scope = scope(binding, alias)?;
        let mut state = self.0.write().map_err(|_| CredentialError::Unavailable)?;
        if !state.available(&scope.0) {
            return Err(CredentialError::Unavailable);
        }
        state.grants.retain(|_, grant| alive(grant.until));
        if state.grants.len() >= 1024 && !state.grants.contains_key(&scope) {
            return Err(CredentialError::Capacity);
        }
        state.ended.remove(&scope);
        state.grants.insert(
            scope.clone(),
            Grant {
                identity: Arc::new(()),
                until,
                lifetime,
                material_generation: self.1.revision()?.0,
                secrets: binding
                    .import()
                    .credential_refs()
                    .values()
                    .map(|r| Ok((r.clone(), self.1.lookup(r)?.as_ref().map(Arc::downgrade))))
                    .collect::<Result<_, CredentialError>>()?,
                references: binding
                    .import()
                    .credential_refs()
                    .values()
                    .cloned()
                    .collect(),
            },
        );
        state.reads.retain(|_, g| g.scope != scope);
        state.read_changed();
        Ok(())
    }
    pub(crate) fn retain_bindings(&self, bindings: &BTreeSet<(String, Revision)>) {
        if let Ok(mut state) = self.0.write() {
            let removed = state
                .grants
                .keys()
                .filter(|s| !bindings.contains(&(s.0.clone(), s.1)))
                .cloned()
                .collect::<Vec<_>>();
            for scope in removed {
                state.end_access(&scope, AccessEnd::Changed);
            }
            state
                .reads
                .retain(|_, g| bindings.contains(&(g.scope.0.clone(), g.scope.1)));
            state.read_changed();
        }
    }
    pub fn revoke(&self, binding: &Binding, alias: &str) -> Result<(), CredentialError> {
        let scope = scope(binding, alias)?;
        let mut state = self.0.write().map_err(|_| CredentialError::Unavailable)?;
        state.end_access(&scope, AccessEnd::Revoked);
        state.read_changed();
        Ok(())
    }
    pub fn disable(&self, name: &str) -> Result<(), CredentialError> {
        if name.is_empty() || name.len() > 128 {
            return Err(CredentialError::InvalidName);
        }
        let mut state = self.0.write().map_err(|_| CredentialError::Unavailable)?;
        if state.disabled.len() >= 128 && !state.disabled.contains(name) {
            return Err(CredentialError::Capacity);
        }
        state.disabled.insert(name.into());
        if let Some(token) = state.lifetimes.remove(name) {
            token.cancel();
        }
        let scopes = state
            .grants
            .keys()
            .filter(|s| s.0 == name)
            .cloned()
            .collect::<Vec<_>>();
        for scope in scopes {
            state.end_access(&scope, AccessEnd::Disabled);
        }
        state.reads.retain(|_, g| g.scope.0 != name);
        state.read_changed();
        if let Some(namespace) = &state.namespace {
            let origin = format!("{namespace}/{name}");
            let destination = format!("{origin}@");
            state.transfers.retain(|(source, target), _| {
                source != &origin && !target.starts_with(&destination)
            });
        }
        Ok(())
    }
    pub fn enable(&self, name: &str) -> Result<(), CredentialError> {
        let mut state = self.0.write().map_err(|_| CredentialError::Unavailable)?;
        if state.closed {
            return Err(CredentialError::Unavailable);
        }
        if name.is_empty() || name.len() > 128 {
            return Err(CredentialError::InvalidName);
        }
        if state.enabled.len() >= 128 && !state.enabled.contains(name) {
            return Err(CredentialError::Capacity);
        }
        state.disabled.remove(name);
        state.enabled.insert(name.into());
        state.read_changed();
        Ok(())
    }
    pub fn available(&self, name: &str) -> bool {
        self.0.read().is_ok_and(|s| s.available(name))
    }
    /// Notification of environment availability only, not a credential/transfer grant.
    /// Each caller owns a child: cancelling one run cannot revoke another. Disable/close
    /// revokes old children permanently, including across an immediate re-enable.
    pub fn availability_lease(&self, name: &str) -> Result<CancellationToken, CredentialError> {
        if name.is_empty() || name.len() > 128 {
            return Err(CredentialError::InvalidName);
        }
        let mut state = self.0.write().map_err(|_| CredentialError::Unavailable)?;
        if !state.available(name) {
            return Err(CredentialError::Unavailable);
        }
        if !state.lifetimes.contains_key(name) && state.lifetimes.len() >= 128 {
            return Err(CredentialError::Capacity);
        }
        Ok(state
            .lifetimes
            .entry(name.into())
            .or_default()
            .child_token())
    }
    pub fn close(&self) {
        if let Ok(mut state) = self.0.write() {
            state.closed = true;
            state.resources = Default::default();
            state.cancel_lifetimes();
            state.grants.clear();
            state.transfers.clear();
            state.reads.clear();
            state.known_scopes.clear();
            state.read_changed();
        }
    }
    pub fn credentials(
        &self,
        binding: &Binding,
        alias: &str,
    ) -> Result<Arc<dyn Credentials>, CredentialError> {
        Ok(Arc::new(Scoped {
            authority: self.clone(),
            scope: scope(binding, alias)?,
            references: binding.import().credential_refs().clone(),
        }))
    }
}
impl State {
    fn cancel_lifetimes(&mut self) {
        for token in std::mem::take(&mut self.lifetimes).into_values() {
            token.cancel();
        }
    }
    fn permits(
        &self,
        policy: &wes_core::flow::FlowPolicy,
        destination: &str,
        revision: Revision,
    ) -> bool {
        !policy.is_unknown()
            && policy.origins().iter().all(|origin| {
                origin == destination
                    || self
                        .transfers
                        .get(&(origin.clone(), format!("{destination}@{revision}")))
                        .is_some_and(|until| *until > Instant::now())
            })
    }
    fn available(&self, name: &str) -> bool {
        !self.closed
            && !self.disabled.contains(name)
            && (!self.restored || self.enabled.contains(name))
    }
}
fn scope(binding: &Binding, alias: &str) -> Result<Scope, CredentialError> {
    if !binding
        .environment()
        .imports()
        .get(alias)
        .is_some_and(|i| Arc::ptr_eq(i, binding.import()))
    {
        return Err(CredentialError::InvalidName);
    }
    Ok((
        binding.environment().identity().into(),
        binding.environment().revision(),
        alias.into(),
    ))
}
struct Scoped {
    authority: Authority,
    scope: Scope,
    references: BTreeMap<String, String>,
}
impl Credentials for Scoped {
    fn snapshot(
        &self,
        names: &[String],
    ) -> Result<BTreeMap<String, Option<Secret>>, CredentialError> {
        if names.len() > 256 {
            return Err(CredentialError::Capacity);
        }
        let state = self
            .authority
            .0
            .read()
            .map_err(|_| CredentialError::Unavailable)?;
        if !state.available(&self.scope.0) {
            return Err(CredentialError::Unavailable);
        }
        if names.is_empty() {
            return Ok(BTreeMap::new());
        }
        let grant = state
            .grants
            .get(&self.scope)
            .ok_or_else(|| CredentialError::AccessDenied(self.scope.2.clone()))?;
        if !grant.current(&self.authority.1) {
            return Err(CredentialError::AccessDenied(self.scope.2.clone()));
        }
        names
            .iter()
            .map(|name| {
                let reference = self
                    .references
                    .get(name)
                    .ok_or(CredentialError::InvalidName)?;
                if !grant.references.contains(reference) {
                    return Err(CredentialError::AccessDenied(self.scope.2.clone()));
                }
                Ok((name.clone(), self.authority.1.lookup(reference)?))
            })
            .collect()
    }
    fn lookup(&self, name: &str) -> Result<Option<Secret>, CredentialError> {
        let reference = self
            .references
            .get(name)
            .ok_or(CredentialError::InvalidName)?;
        let state = self
            .authority
            .0
            .read()
            .map_err(|_| CredentialError::Unavailable)?;
        if !state.available(&self.scope.0) {
            return Err(CredentialError::Unavailable);
        }
        let grant = state
            .grants
            .get(&self.scope)
            .ok_or_else(|| CredentialError::AccessDenied(self.scope.2.clone()))?;
        if !grant.current(&self.authority.1) || !grant.references.contains(reference) {
            return Err(CredentialError::AccessDenied(self.scope.2.clone()));
        }
        self.authority.1.lookup(reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::ExposeSecret;
    use wes_core::environments::{CapturedSource, CapturedSources, Package};
    pub(super) fn bindings() -> (Binding, Binding) {
        let package = Package::parse("version: 1\ntargets: {local: {kind: local}}\nenvironments:\n  dev: {secretSlots: {token: {required: true}}, secretRefs: {token: dev/key}, imports: {api: {source: {kind: spec, file: api.json}, bind: {target: local, credentials: {key: {secret: token}}}}}}\n  prod: {extends: {env: dev, track: latest}, secretRefs: {token: prod/key}}\n").unwrap();
        let mut sources = CapturedSources::default();
        for key in package.required_sources() {
            sources
                .insert(key, CapturedSource::new("qa/v1", "{}").unwrap())
                .unwrap();
        }
        let mut registry = crate::environments::Registry::default();
        let plan = registry.plan(&package, &sources).unwrap();
        registry.apply(plan).unwrap();
        (
            registry.inspect("dev").unwrap().bind("api").unwrap(),
            registry.inspect("prod").unwrap().bind("api").unwrap(),
        )
    }
    #[test]
    fn expired_grants_are_access_denials_and_closed_authority_remains_unavailable() {
        let (dev, _) = bindings();
        let authority = Authority::default();
        let port = authority.credentials(&dev, "api").unwrap();
        authority
            .grant(&dev, "api", AccessLifetime::Timed { seconds: 60 })
            .unwrap();
        authority
            .0
            .write()
            .unwrap()
            .grants
            .values_mut()
            .next()
            .unwrap()
            .until = Some(Instant::now());
        assert_eq!(
            port.lookup("key").unwrap_err(),
            CredentialError::AccessDenied("api".into())
        );
        assert_eq!(
            port.snapshot(&["key".into()]).unwrap_err(),
            CredentialError::AccessDenied("api".into())
        );
        authority.0.write().unwrap().closed = true;
        assert_eq!(
            port.lookup("key").unwrap_err(),
            CredentialError::Unavailable
        );
    }

    #[test]
    fn scoped_material_grants_rotation_revocation_and_recovery_are_separate() {
        let (dev, prod) = bindings();
        let authority = Authority::default();
        let a = authority.credentials(&dev, "api").unwrap();
        let b = authority.credentials(&prod, "api").unwrap();
        authority
            .supply(
                "dev/key".into(),
                Arc::new(crate::credentials::SecretString::from("qa-one")),
            )
            .unwrap();
        assert_eq!(
            a.lookup("key").unwrap_err(),
            CredentialError::AccessDenied("api".into())
        );
        assert_eq!(
            a.snapshot(&["key".into()]).unwrap_err(),
            CredentialError::AccessDenied("api".into())
        );
        authority
            .grant(&dev, "api", AccessLifetime::Timed { seconds: 60 })
            .unwrap();
        let old = a.lookup("key").unwrap().unwrap();
        assert!(b.lookup("key").is_err());
        assert!(a.lookup("other").is_err());
        authority
            .supply(
                "dev/key".into(),
                Arc::new(crate::credentials::SecretString::from("qa-two")),
            )
            .unwrap();
        assert_eq!(old.expose_secret(), "qa-one");
        assert!(a.lookup("key").is_err());
        authority
            .grant(&dev, "api", AccessLifetime::Timed { seconds: 60 })
            .unwrap();
        assert_eq!(a.lookup("key").unwrap().unwrap().expose_secret(), "qa-two");
        authority.revoke(&dev, "api").unwrap();
        assert!(a.lookup("key").is_err());
        authority
            .grant(&dev, "api", AccessLifetime::Timed { seconds: 60 })
            .unwrap();
        authority.forget("dev/key").unwrap();
        assert!(a.lookup("key").is_err());
        authority.disable("dev").unwrap();
        assert!(!authority.available("dev"));
        assert!(authority.available("prod"));
        authority.enable("dev").unwrap();
        assert!(a.lookup("key").is_err());
        authority.restored();
        assert!(!authority.available("prod"));
        authority.enable("prod").unwrap();
        assert!(authority.available("prod"));
        assert!(b.lookup("key").is_err());
        authority.close();
        assert!(authority.enable("prod").is_err());
        assert!(a.snapshot(&[]).is_err());
    }
    #[test]
    fn transfer_requires_exact_destination_revision_and_never_declassifies() {
        let (dev, prod) = bindings();
        let authority = Authority::default();
        authority.establish_namespace("00000000-0000-4000-8000-000000000001");
        let from = authority.origin("dev").unwrap();
        let to = authority.origin("prod").unwrap();
        let policy = wes_core::flow::FlowPolicy::default()
            .from_origin(from.clone())
            .private();
        assert!(!authority.permits(&policy, &to, prod.environment().revision()));
        authority
            .transfer(
                from,
                format!("{to}@{}", prod.environment().revision()),
                Duration::from_secs(60),
            )
            .unwrap();
        assert!(authority.permits(&policy, &to, prod.environment().revision()));
        assert!(!authority.permits(&policy, &to, dev.environment().revision()));
        assert!(policy.is_private());
        assert!(!authority.permits(
            &policy.clone().unknown(),
            &to,
            prod.environment().revision()
        ));
        authority
            .0
            .write()
            .unwrap()
            .transfers
            .values_mut()
            .for_each(|until| *until = Instant::now());
        assert!(!authority.permits(&policy, &to, prod.environment().revision()));
    }
}

#[cfg(test)]
mod lifetime_tests {
    use super::*;
    #[test]
    fn availability_leases_are_scoped_revocable_and_never_reanimated() {
        let authority = Authority::default();
        let first = authority.availability_lease("dev").unwrap();
        let sibling = authority.availability_lease("dev").unwrap();
        let other = authority.availability_lease("other").unwrap();
        first.cancel();
        assert!(!sibling.is_cancelled());
        authority.disable("dev").unwrap();
        authority.enable("dev").unwrap();
        assert!(sibling.is_cancelled());
        assert!(!other.is_cancelled());
        let fresh = authority.availability_lease("dev").unwrap();
        assert!(!fresh.is_cancelled());
        authority.close();
        assert!(fresh.is_cancelled() && other.is_cancelled());
        assert!(authority.availability_lease("dev").is_err());
    }
    #[test]
    fn restored_authority_revokes_leases_and_lease_capacity_is_explicit() {
        let authority = Authority::default();
        let lease = authority.availability_lease("dev").unwrap();
        authority.restored();
        assert!(lease.is_cancelled());
        assert!(authority.availability_lease("dev").is_err());
        authority.enable("dev").unwrap();
        assert!(!authority.availability_lease("dev").unwrap().is_cancelled());
        let authority = Authority::default();
        for n in 0..128 {
            authority.availability_lease(&format!("e{n}")).unwrap();
        }
        assert!(matches!(
            authority.availability_lease("overflow"),
            Err(CredentialError::Capacity)
        ));
        authority.disable("e0").unwrap();
        assert!(authority.availability_lease("overflow").is_ok());
    }
    #[test]
    fn dispatch_refusals_distinguish_restore_disable_close_and_transfer() {
        for reason in [
            DispatchDenied::Closed,
            DispatchDenied::Disabled,
            DispatchDenied::Restored,
            DispatchDenied::Unavailable,
            DispatchDenied::Transfer,
        ] {
            assert_eq!(reason.code(), "ENV020");
            assert!(!reason.message().contains("ENV020:"));
        }
        let name = "dev";
        let revision = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            .parse()
            .unwrap();
        let policy = wes_core::flow::FlowPolicy::default();
        let authority = Authority::default();
        assert_eq!(
            authority.require_dispatch(name, &policy, revision),
            Err(DispatchDenied::Unavailable)
        );
        authority.establish_namespace("fixture");
        assert_eq!(authority.require_dispatch(name, &policy, revision), Ok(()));
        authority.restored();
        assert_eq!(
            authority.require_dispatch(name, &policy, revision),
            Err(DispatchDenied::Restored)
        );
        authority.enable(name).unwrap();
        assert_eq!(authority.require_dispatch(name, &policy, revision), Ok(()));
        authority.disable(name).unwrap();
        assert_eq!(
            authority.require_dispatch(name, &policy, revision),
            Err(DispatchDenied::Disabled)
        );
        authority.enable(name).unwrap();
        assert_eq!(
            authority.require_dispatch(name, &policy.from_origin("foreign"), revision),
            Err(DispatchDenied::Transfer)
        );
        authority.close();
        assert_eq!(
            authority.require_dispatch(name, &wes_core::flow::FlowPolicy::default(), revision),
            Err(DispatchDenied::Closed)
        );
    }
}
