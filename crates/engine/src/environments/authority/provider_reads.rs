//! Captured scope proof and its revocable process-local read grant. Credentials,
//! execution availability and reading previously captured data are distinct.
use super::*;
use sha2::{Digest, Sha256};
use wes_core::flow::ProviderReadOrigin;
#[derive(Clone, Debug, serde::Serialize)]
pub struct ReadScopeStatus {
    pub scope: BTreeMap<String, String>,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft: Option<String>,
}
#[derive(Clone)]
pub(super) struct KnownScope {
    pub(super) binding: Scope,
    pub(super) labels: BTreeMap<String, String>,
    pub(super) draft: Option<String>,
}
pub(super) struct ReadGrant {
    identity: Arc<()>,
    pub(super) scope: Scope,
    until: Option<Instant>,
    grant: Option<Arc<()>>,
    material: u64,
    secrets: BTreeMap<String, Secret>,
}
/// One live proof generation. A fresh proof cannot revive work admitted by an old one.
#[derive(Clone)]
pub(crate) struct ReadPermit {
    authority: Authority,
    origin: ProviderReadOrigin,
    identity: Arc<()>,
}
impl ReadPermit {
    pub(crate) fn current(&self) -> bool {
        self.authority.0.read().is_ok_and(|state| {
            state.reads.get(&self.origin).is_some_and(|grant| {
                Arc::ptr_eq(&grant.identity, &self.identity)
                    && grant.current(&state, &self.authority.1)
            })
        })
    }
}
/// Non-serializable receipt captured before one explicit bounded probe. Finishing
/// it is possible only on the originating owner and current authority epoch.
pub struct ReadProbe {
    authority: Authority,
    origin: ProviderReadOrigin,
    proof: ReadGrant,
}
impl fmt::Debug for ReadProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ReadProbe(..)")
    }
}
impl State {
    pub(super) fn read_changed(&mut self) {
        // Authority changes invalidate every pending attempt, including an
        // old attempt held across disable/enable or revoke/regrant.
        self.read_probes.clear();
        self.notify_reads();
    }
    fn probe_current(&self, origin: &ProviderReadOrigin, identity: &Arc<()>) -> bool {
        self.read_probes
            .get(origin)
            .and_then(std::sync::Weak::upgrade)
            .is_some_and(|current| Arc::ptr_eq(&current, identity))
    }
    fn notify_reads(&mut self) {
        let Some(next) = self.read_epoch.checked_add(1) else {
            self.closed = true;
            self.reads.clear();
            self.read_probes.clear();
            return;
        };
        self.read_epoch = next;
        if let Some(updates) = &self.read_updates {
            updates.send_replace(next);
        }
        self.read_observers.retain(|p| {
            if let Some(p) = p.upgrade() {
                p.changed();
                true
            } else {
                false
            }
        });
    }
}
impl Authority {
    pub(super) fn read_scope_status(&self, state: &State, scope: &Scope) -> Vec<ReadScopeStatus> {
        state
            .known_scopes
            .iter()
            .filter(|(_, known)| &known.binding == scope)
            .map(|(origin, known)| ReadScopeStatus {
                scope: if known.labels.is_empty() {
                    BTreeMap::from([("scope digest".into(), origin.scope().to_owned())])
                } else {
                    known.labels.clone()
                },
                state: if state
                    .reads
                    .get(origin)
                    .is_some_and(|g| g.current(state, &self.1))
                {
                    "verified"
                } else if state.grants.get(scope).is_some_and(|g| g.current(&self.1)) {
                    "unverified"
                } else {
                    "lapsed"
                },
                draft: known.draft.clone(),
            })
            .collect()
    }
    pub fn read_updates(&self) -> Result<tokio::sync::watch::Receiver<u64>, CredentialError> {
        let mut state = self.0.write().map_err(|_| CredentialError::Unavailable)?;
        let epoch = state.read_epoch;
        Ok(state
            .read_updates
            .get_or_insert_with(|| tokio::sync::watch::channel(epoch).0)
            .subscribe())
    }
    pub(crate) fn read_deadline(&self) -> Option<Instant> {
        self.0
            .read()
            .ok()?
            .reads
            .values()
            .filter_map(|g| g.until)
            .min()
    }
    pub(crate) fn expire_reads(&self) {
        if let Ok(mut state) = self.0.write() {
            let before = state.reads.len();
            state.reads.retain(|_, g| {
                alive(g.until) && self.1.revision().ok().map(|r| r.0) == Some(g.material)
            });
            if before != state.reads.len() {
                state.read_changed();
            }
        }
    }
    pub(super) fn invalidate_read_material(&self, reference: &str) {
        if let Ok(mut state) = self.0.write() {
            let scopes = state
                .grants
                .iter()
                .filter(|(_, g)| g.references.contains(reference))
                .map(|(s, _)| s.clone())
                .collect::<Vec<_>>();
            for scope in scopes {
                state.end_access(&scope, AccessEnd::Changed);
            }
            state
                .reads
                .retain(|_, g| !g.secrets.contains_key(reference));
            state.read_changed();
        }
    }
    pub fn provider_read_origin(
        &self,
        binding: &Binding,
        alias: &str,
        scope_digest: &str,
    ) -> Result<ProviderReadOrigin, CredentialError> {
        let captured = scope(binding, alias)?;
        let state = self.0.read().map_err(|_| CredentialError::Unavailable)?;
        let realm = state
            .namespace
            .clone()
            .ok_or(CredentialError::Unavailable)?;
        let mut hash = Sha256::new();
        hash.update(b"wes.provider-read-binding/v1");
        for s in [captured.0, captured.1.to_string(), captured.2] {
            hash.update((s.len() as u64).to_be_bytes());
            hash.update(s.as_bytes());
        }
        ProviderReadOrigin::new(realm, format!("{:x}", hash.finalize()), scope_digest.into())
            .map_err(|_| CredentialError::InvalidName)
    }
    pub fn begin_read_probe(
        &self,
        binding: &Binding,
        alias: &str,
        scope_digest: &str,
        principal: &InvocationAuthority,
    ) -> Result<ReadProbe, CredentialError> {
        if !principal.is_local_user() {
            return Err(CredentialError::AccessDenied(alias.into()));
        }
        let origin = self.provider_read_origin(binding, alias, scope_digest)?;
        let scope = scope(binding, alias)?;
        let mut state = self.0.write().map_err(|_| CredentialError::Unavailable)?;
        if !state.available(&scope.0) {
            return Err(CredentialError::Unavailable);
        }
        let (until, grant) =
            if let Some(grant) = state.grants.get(&scope).filter(|g| g.current(&self.1)) {
                if binding
                    .import()
                    .credential_refs()
                    .values()
                    .any(|r| !grant.references.contains(r))
                {
                    return Err(CredentialError::AccessDenied(alias.into()));
                }
                (grant.until, Some(grant.identity.clone()))
            } else if binding.import().credential_refs().is_empty() {
                // An explicit native probe without credentials starts a default finite lifetime.
                // Only its successful outcome proves this scope; it never opts into Session.
                state.grants.retain(|_, g| alive(g.until));
                if state.grants.len() >= 1024 && !state.grants.contains_key(&scope) {
                    return Err(CredentialError::Capacity);
                }
                let until = AccessLifetime::DEFAULT.deadline()?;
                let identity = Arc::new(());
                state.ended.remove(&scope);
                state.grants.insert(
                    scope.clone(),
                    Grant {
                        identity: identity.clone(),
                        until,
                        lifetime: AccessLifetime::DEFAULT,
                        material_generation: self.1.revision()?.0,
                        references: Default::default(),
                        secrets: Default::default(),
                    },
                );
                (until, Some(identity))
            } else {
                return Err(CredentialError::AccessDenied(alias.into()));
            };
        let mut secrets = BTreeMap::new();
        for reference in binding.import().credential_refs().values() {
            let secret = self
                .1
                .lookup(reference)?
                .ok_or_else(|| CredentialError::AccessDenied(alias.into()))?;
            secrets.insert(reference.clone(), secret);
        }
        let material = self.1.revision()?.0;
        // A validation withdraws only its own scope. Distinct scopes may finish
        // independently; a later attempt for the same scope supersedes this one.
        state.reads.remove(&origin);
        state
            .read_probes
            .retain(|_, claim| claim.strong_count() > 0);
        state.notify_reads();
        if state.closed {
            return Err(CredentialError::Unavailable);
        }
        if state.reads.len() + state.read_probes.len()
            >= wes_budgets::get("provider.read.grants") as usize
            && !state.read_probes.contains_key(&origin)
        {
            return Err(CredentialError::Capacity);
        }
        let identity = Arc::new(());
        state
            .read_probes
            .insert(origin.clone(), Arc::downgrade(&identity));
        Ok(ReadProbe {
            authority: self.clone(),
            origin,
            proof: ReadGrant {
                identity,
                scope,
                until,
                grant,
                material,
                secrets,
            },
        })
    }
    pub fn permits_provider_read(&self, origin: &ProviderReadOrigin) -> bool {
        let Ok(state) = self.0.read() else {
            return false;
        };
        if state.namespace.as_deref() != Some(origin.realm()) {
            return false;
        }
        state
            .reads
            .get(origin)
            .is_some_and(|g| g.current(&state, &self.1))
    }
    pub(crate) fn capture_read_permit(&self, origin: &ProviderReadOrigin) -> Option<ReadPermit> {
        let state = self.0.read().ok()?;
        if state.namespace.as_deref() != Some(origin.realm()) {
            return None;
        }
        let grant = state
            .reads
            .get(origin)
            .filter(|g| g.current(&state, &self.1))?;
        Some(ReadPermit {
            authority: self.clone(),
            origin: origin.clone(),
            identity: grant.identity.clone(),
        })
    }
}
impl ReadGrant {
    fn current(&self, state: &State, material: &crate::credentials::material::Material) -> bool {
        state.available(&self.scope.0)
            && alive(self.until)
            && material.revision().ok().map(|r| r.0) == Some(self.material)
            && self.grant.as_ref().is_none_or(|id| {
                state
                    .grants
                    .get(&self.scope)
                    .is_some_and(|g| g.current(material) && Arc::ptr_eq(id, &g.identity))
            })
            && self.secrets.iter().all(|(r, s)| {
                material
                    .lookup(r)
                    .is_ok_and(|v| v.is_some_and(|v| Arc::ptr_eq(s, &v)))
            })
    }
}
impl ReadProbe {
    pub(crate) fn describe(&self, labels: BTreeMap<String, String>, draft: Option<String>) {
        if let Ok(mut state) = self.authority.0.write() {
            if !state.probe_current(&self.origin, &self.proof.identity)
                || labels.len() > 16
                || labels.iter().any(|(k, v)| k.len() > 128 || v.len() > 1024)
                || draft.as_ref().is_some_and(|s| s.len() > 20000)
            {
                return;
            }
            if state.known_scopes.len() >= wes_budgets::get("provider.read.grants") as usize
                && !state.known_scopes.contains_key(&self.origin)
            {
                state.known_scopes.pop_first();
            }
            state.known_scopes.insert(
                self.origin.clone(),
                KnownScope {
                    binding: self.proof.scope.clone(),
                    labels,
                    draft,
                },
            );
        }
    }
    pub(crate) fn current(&self) -> bool {
        self.authority.0.read().is_ok_and(|s| {
            s.probe_current(&self.origin, &self.proof.identity)
                && s.namespace.as_deref() == Some(self.origin.realm())
                && self.proof.current(&s, &self.authority.1)
        })
    }
    /// Call only after the physically joined, contract-checked probe returned Bool
    /// true. False, cancellation, failed/unknown outcomes never open a read realm.
    pub(crate) fn confirm(self) -> Result<(), CredentialError> {
        let mut state = self
            .authority
            .0
            .write()
            .map_err(|_| CredentialError::Unavailable)?;
        if !state.probe_current(&self.origin, &self.proof.identity)
            || state.namespace.as_deref() != Some(self.origin.realm())
            || !self.proof.current(&state, &self.authority.1)
        {
            return Err(CredentialError::Unavailable);
        }
        if state.reads.len() >= wes_budgets::get("provider.read.grants") as usize
            && !state.reads.contains_key(&self.origin)
        {
            return Err(CredentialError::Capacity);
        }
        state.read_probes.remove(&self.origin);
        state.reads.insert(self.origin, self.proof);
        state.notify_reads();
        Ok(())
    }
}
impl crate::provider_reads::ReadAuthority for Authority {
    fn observe(
        &self,
        observer: std::sync::Weak<crate::provider_reads::Changes>,
    ) -> Result<(), &'static str> {
        self.1
            .observe(observer.clone())
            .map_err(|_| "provider read notification is unavailable")?;
        let mut state = self
            .0
            .write()
            .map_err(|_| "provider read owner is unavailable")?;
        state.read_observers.retain(|p| p.strong_count() > 0);
        if observer.strong_count() == 0 || state.read_observers.iter().any(|p| p.ptr_eq(&observer))
        {
            return Ok(());
        }
        if state.read_observers.len() >= 128 {
            return Err("provider read notification capacity exceeded");
        }
        state.read_observers.push(observer);
        Ok(())
    }
    fn deadline(&self) -> Option<Instant> {
        self.read_deadline()
    }
    fn expire(&self) {
        self.expire_reads();
    }
    fn identity(&self) -> String {
        format!("{:p}", Arc::as_ptr(&self.0))
    }
    fn realm(&self) -> Option<String> {
        let s = self.0.read().ok()?;
        if s.closed { None } else { s.namespace.clone() }
    }
    fn closed(&self) -> bool {
        self.0.read().map_or(true, |s| s.closed)
    }
    fn revision(&self) -> String {
        format!(
            "{}:{:?}",
            self.0.read().map_or(u64::MAX, |s| s.read_epoch),
            self.1.revision()
        )
    }
    fn allows(&self, origin: &ProviderReadOrigin) -> bool {
        self.permits_provider_read(origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{provider_reads::Readers, source::SourceInput};
    fn native() -> InvocationAuthority {
        InvocationAuthority::from_source(
            &SourceInput::new("native-probe".into(), "synthetic".into()).unwrap(),
        )
    }
    fn fixture() -> (Authority, Binding) {
        let (binding, _) = super::super::tests::bindings();
        let authority = Authority::default();
        authority.establish_namespace("00000000-0000-4000-8000-000000000031");
        authority
            .supply(
                "dev/key".into(),
                Arc::new(crate::credentials::SecretString::from("synthetic")),
            )
            .unwrap();
        authority
            .grant(&binding, "api", AccessLifetime::Timed { seconds: 60 })
            .unwrap();
        (authority, binding)
    }
    fn open(authority: &Authority, binding: &Binding, scope: char) -> ProviderReadOrigin {
        let probe = authority
            .begin_read_probe(binding, "api", &scope.to_string().repeat(64), &native())
            .unwrap();
        let origin = probe.origin.clone();
        probe.confirm().unwrap();
        origin
    }
    #[test]
    fn read_proofs_use_the_selected_deadline_and_session_grants_have_none() {
        let (a, b) = fixture();
        a.grant(&b, "api", AccessLifetime::Timed { seconds: 14400 })
            .unwrap();
        let origin = open(&a, &b, 'a');
        let permit = a.capture_read_permit(&origin).unwrap();
        {
            let s = a.0.read().unwrap();
            let deadline = s.grants.get(&scope(&b, "api").unwrap()).unwrap().until;
            assert_eq!(s.reads[&origin].until, deadline);
            assert!(deadline.unwrap().duration_since(Instant::now()) > Duration::from_secs(14300));
        }
        a.grant(&b, "api", AccessLifetime::SESSION).unwrap();
        assert!(!a.permits_provider_read(&origin));
        assert!(!permit.current());
        assert!(matches!(
            a.status(&b, "api").unwrap().access,
            AccessStatus::Session
        ));
        assert_eq!(open(&a, &b, 'a'), origin);
        assert_eq!(a.read_deadline(), None);
        assert!(!permit.current());
        a.disable("dev").unwrap();
        a.enable("dev").unwrap();
        assert!(!a.permits_provider_read(&origin));
        a.grant(&b, "api", AccessLifetime::SESSION).unwrap();
        open(&a, &b, 'a');
        a.restored();
        a.enable("dev").unwrap();
        assert!(!a.status(&b, "api").unwrap().access.granted());
        assert!(!a.permits_provider_read(&origin));
    }
    #[test]
    fn scope_inspection_never_grants_and_binding_changes_revoke_session_proofs() {
        let (a, b) = fixture();
        a.grant(&b, "api", AccessLifetime::SESSION).unwrap();
        let p = a
            .begin_read_probe(&b, "api", &"a".repeat(64), &native())
            .unwrap();
        p.describe(
            BTreeMap::from([("region".into(), "north".into())]),
            Some("api probe region:\"north\"".into()),
        );
        let origin = p.origin.clone();
        assert_eq!(
            a.status(&b, "api").unwrap().read_scopes[0].state,
            "unverified"
        );
        assert!(!a.permits_provider_read(&origin));
        p.confirm().unwrap();
        assert_eq!(
            a.status(&b, "api").unwrap().read_scopes[0].state,
            "verified"
        );
        a.retain_bindings(&Default::default());
        assert!(!a.permits_provider_read(&origin));
        assert_eq!(a.status(&b, "api").unwrap().read_scopes[0].state, "lapsed");
        a.close();
        assert!(a.status(&b, "api").unwrap().read_scopes.is_empty());
    }
    #[test]
    fn regrant_and_revalidation_do_not_revive_old_work_or_other_scopes() {
        let (a, b) = fixture();
        let first = open(&a, &b, 'a');
        let other = open(&a, &b, 'b');
        let permit = a.capture_read_permit(&first).unwrap();
        let readers = Readers::default();
        readers.register(Arc::new(a.clone())).unwrap();
        let policy = wes_core::flow::FlowPolicy::default()
            .with_provider_read(first.clone())
            .with_provider_read(other.clone());
        assert!(!readers.blocks_policy(&policy));
        let epoch = readers.epoch();
        a.revoke(&b, "api").unwrap();
        a.grant(&b, "api", AccessLifetime::Timed { seconds: 60 })
            .unwrap();
        assert!(readers.blocks_policy(&policy));
        assert!(!permit.current());
        assert_eq!(open(&a, &b, 'a'), first);
        assert!(a.permits_provider_read(&first));
        assert!(!a.permits_provider_read(&other));
        assert!(readers.blocks_policy(&policy));
        assert!(!permit.current());
        assert_ne!(epoch, readers.epoch());
        open(&a, &b, 'b');
        assert!(!readers.blocks_policy(&policy));
        // A second live owner for a realm is ambiguous, never a grant.
        let (duplicate, _) = fixture();
        readers.register(Arc::new(duplicate)).unwrap();
        assert!(readers.blocks_policy(&policy));
        assert!(!readers.blocks_policy(&Default::default()));
    }
    #[test]
    fn independent_read_scopes_can_finish_in_either_order() {
        for reverse in [false, true] {
            let (authority, binding) = fixture();
            let first = authority
                .begin_read_probe(&binding, "api", &"a".repeat(64), &native())
                .unwrap();
            let second = authority
                .begin_read_probe(&binding, "api", &"b".repeat(64), &native())
                .unwrap();
            let a = authority
                .provider_read_origin(&binding, "api", &"a".repeat(64))
                .unwrap();
            let b = authority
                .provider_read_origin(&binding, "api", &"b".repeat(64))
                .unwrap();
            if reverse {
                second.confirm().unwrap();
                first.confirm().unwrap();
            } else {
                first.confirm().unwrap();
                second.confirm().unwrap();
            }
            assert!(authority.permits_provider_read(&a));
            assert!(authority.permits_provider_read(&b));
        }
    }

    #[test]
    fn pending_read_scopes_share_a_bound_and_abandoned_claims_release_capacity() {
        let (authority, binding) = fixture();
        let limit = wes_budgets::get("provider.read.grants") as usize;
        let mut pending = (0..limit)
            .map(|n| {
                authority
                    .begin_read_probe(&binding, "api", &format!("{n:064x}"), &native())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            authority.begin_read_probe(&binding, "api", &format!("{limit:064x}"), &native()),
            Err(CredentialError::Capacity)
        ));
        drop(pending.pop());
        let next = authority
            .begin_read_probe(&binding, "api", &format!("{limit:064x}"), &native())
            .unwrap();
        next.confirm().unwrap();
        // Confirming consumes its reservation, rather than freeing an extra slot.
        assert!(matches!(
            authority.begin_read_probe(&binding, "api", &format!("{:064x}", limit + 1), &native()),
            Err(CredentialError::Capacity)
        ));
        authority.disable("dev").unwrap();
        authority.enable("dev").unwrap();
        authority
            .grant(
                &binding,
                "api",
                super::AccessLifetime::Timed { seconds: 60 },
            )
            .unwrap();
        for old in pending {
            assert!(old.confirm().is_err());
        }
    }

    #[test]
    fn abandoned_overlapping_cancelled_or_old_credential_probe_cannot_open() {
        let (a, b) = fixture();
        let origin = open(&a, &b, 'a');
        let old = a
            .begin_read_probe(&b, "api", &"a".repeat(64), &native())
            .unwrap();
        assert!(!a.permits_provider_read(&origin));
        let newest = a
            .begin_read_probe(&b, "api", &"a".repeat(64), &native())
            .unwrap();
        assert!(old.confirm().is_err());
        drop(newest); // false, failed, cancelled or unknown: no confirmation.
        assert!(!a.permits_provider_read(&origin));
        let pending = a
            .begin_read_probe(&b, "api", &"a".repeat(64), &native())
            .unwrap();
        a.supply(
            "dev/key".into(),
            Arc::new(crate::credentials::SecretString::from("synthetic")),
        )
        .unwrap();
        assert!(pending.confirm().is_err());
        let actor = InvocationAuthority::from_source(
            &SourceInput::new("actor".into(), "synthetic".into())
                .unwrap()
                .cooperative(),
        );
        assert!(
            a.begin_read_probe(&b, "api", &"a".repeat(64), &actor)
                .is_err()
        );
        assert!(
            a.begin_read_probe(&b, "api", &"a".repeat(64), &Default::default())
                .is_err()
        );
    }
    #[test]
    fn expiry_disable_recovery_rotation_and_foreign_bindings_fail_closed() {
        let (a, b) = fixture();
        let origin = open(&a, &b, 'a');
        let (_, foreign) = super::super::tests::bindings();
        let different = a
            .provider_read_origin(&foreign, "api", &"a".repeat(64))
            .unwrap();
        assert!(!a.permits_provider_read(&different));
        a.0.write().unwrap().reads.get_mut(&origin).unwrap().until = Some(Instant::now());
        assert!(!a.permits_provider_read(&origin));
        a.expire_reads();
        open(&a, &b, 'a');
        a.disable("dev").unwrap();
        a.enable("dev").unwrap();
        assert!(!a.permits_provider_read(&origin));
        a.grant(&b, "api", AccessLifetime::Timed { seconds: 60 })
            .unwrap();
        open(&a, &b, 'a');
        a.restored();
        a.enable("dev").unwrap();
        assert!(!a.permits_provider_read(&origin));
        a.grant(&b, "api", AccessLifetime::Timed { seconds: 60 })
            .unwrap();
        open(&a, &b, 'a');
        a.forget("dev/key").unwrap();
        assert!(!a.permits_provider_read(&origin));
        a.close();
        assert!(
            a.begin_read_probe(&b, "api", &"a".repeat(64), &native())
                .is_err()
        );
    }
}
