use super::*;

/// A native, process-local grant lifetime. Session is an explicit absence of a deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum AccessLifetime {
    Timed { seconds: u64 },
    Session(SessionLifetime),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionLifetime {
    Session,
}
impl AccessLifetime {
    pub const SESSION: Self = Self::Session(SessionLifetime::Session);
    pub const DEFAULT: Self = Self::Timed { seconds: 900 };
    pub fn deadline(self) -> Result<Option<Instant>, CredentialError> {
        match self {
            Self::Timed { seconds } if (60..=86400).contains(&seconds) && seconds % 60 == 0 => {
                Instant::now()
                    .checked_add(Duration::from_secs(seconds))
                    .map(Some)
                    .ok_or(CredentialError::Unavailable)
            }
            Self::Session(_) => Ok(None),
            _ => Err(CredentialError::Unavailable),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AccessEnd {
    Expired,
    Revoked,
    Disabled,
    Changed,
}
#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum AccessStatus {
    #[default]
    None,
    #[serde(rename = "none")]
    Ended {
        ended: AccessEnd,
    },
    Timed {
        seconds: u64,
    },
    Session,
}
impl AccessStatus {
    pub fn granted(&self) -> bool {
        matches!(self, Self::Timed { .. } | Self::Session)
    }
}
pub(super) fn alive(until: Option<Instant>) -> bool {
    until.is_none_or(|at| at > Instant::now())
}
impl State {
    pub(super) fn end_access(&mut self, scope: &Scope, reason: AccessEnd) {
        self.grants.remove(scope);
        self.reads.retain(|_, g| &g.scope != scope);
        if self.ended.len() >= 1024 && !self.ended.contains_key(scope) {
            self.ended.pop_first();
        }
        self.ended.insert(scope.clone(), reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durations_are_explicit_bounded_whole_minutes_and_malformed_inputs_fail() {
        for seconds in [0, 1, 59, 61, 86401, u64::MAX] {
            assert!(AccessLifetime::Timed { seconds }.deadline().is_err());
        }
        for seconds in [60, 300, 900, 3600, 14400, 86400] {
            assert!(
                AccessLifetime::Timed { seconds }
                    .deadline()
                    .unwrap()
                    .is_some()
            );
        }
        for source in [
            "null",
            "0",
            "true",
            "\"unlimited\"",
            "{}",
            "{\"seconds\":1.5}",
            "{\"seconds\":60,\"extra\":true}",
        ] {
            assert!(
                serde_json::from_str::<AccessLifetime>(source).is_err(),
                "{source}"
            );
        }
        assert_eq!(
            serde_json::from_str::<AccessLifetime>("\"session\"").unwrap(),
            AccessLifetime::SESSION
        );
        assert_eq!(AccessLifetime::SESSION.deadline().unwrap(), None);
    }
    #[test]
    fn shared_material_rotation_revokes_grants_in_every_live_owner() {
        let (binding, _) = super::super::tests::bindings();
        let material = crate::credentials::material::Material::default();
        let a = Authority::with_material(material.clone());
        let b = Authority::with_material(material);
        a.supply(
            "dev/key".into(),
            Arc::new(crate::credentials::SecretString::from("first")),
        )
        .unwrap();
        for owner in [&a, &b] {
            owner
                .grant(&binding, "api", AccessLifetime::SESSION)
                .unwrap();
        }
        b.supply(
            "dev/key".into(),
            Arc::new(crate::credentials::SecretString::from("second")),
        )
        .unwrap();
        for owner in [&a, &b] {
            assert!(!owner.status(&binding, "api").unwrap().access.granted());
            assert!(
                owner
                    .credentials(&binding, "api")
                    .unwrap()
                    .lookup("key")
                    .is_err()
            );
        }
        a.grant(&binding, "api", AccessLifetime::SESSION).unwrap();
        assert!(a.status(&binding, "api").unwrap().access.granted());
        assert!(!b.status(&binding, "api").unwrap().access.granted());
        a.forget("dev/key").unwrap();
        assert!(!a.status(&binding, "api").unwrap().access.granted());
    }
    #[test]
    fn secure_store_generation_changes_never_revive_a_session_grant() {
        use std::sync::atomic::{AtomicU64, Ordering};
        struct Store {
            value: Secret,
            generation: AtomicU64,
        }
        impl crate::credentials::material::SecureStore for Store {
            fn supported(&self) -> bool {
                true
            }
            fn generation(&self) -> u64 {
                self.generation.load(Ordering::SeqCst)
            }
            fn get(&self, _: &str) -> Result<Option<Secret>, CredentialError> {
                Ok(Some(self.value.clone()))
            }
            fn set(&self, _: &str, _: &Secret) -> Result<(), CredentialError> {
                unreachable!()
            }
            fn remove(&self, _: &str) -> Result<(), CredentialError> {
                unreachable!()
            }
        }
        let (binding, _) = super::super::tests::bindings();
        let store = Arc::new(Store {
            value: Arc::new(crate::credentials::SecretString::from("synthetic-saved")),
            generation: AtomicU64::new(0),
        });
        let a =
            Authority::with_material(crate::credentials::material::Material::new(store.clone()));
        a.hydrate(&binding).unwrap();
        a.grant(&binding, "api", AccessLifetime::SESSION).unwrap();
        assert!(a.status(&binding, "api").unwrap().access.granted());
        store.generation.fetch_add(1, Ordering::SeqCst);
        assert!(!a.status(&binding, "api").unwrap().access.granted());
        // Even a store returning the same allocation after its state changed cannot revive access.
        a.hydrate(&binding).unwrap();
        assert!(!a.status(&binding, "api").unwrap().access.granted());
        assert!(
            a.credentials(&binding, "api")
                .unwrap()
                .lookup("key")
                .is_err()
        );
        a.grant(&binding, "api", AccessLifetime::SESSION).unwrap();
        assert!(a.status(&binding, "api").unwrap().access.granted());
    }
}
