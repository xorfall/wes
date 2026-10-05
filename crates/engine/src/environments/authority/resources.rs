//! Live external-resource rights. Public result IDs and work grants are not capabilities.
use super::*;
use crate::source::SourceInput;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InvocationAuthority(Principal);
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Principal {
    #[default]
    Unknown,
    User,
    Actor(String),
}
impl InvocationAuthority {
    pub fn is_local_user(&self) -> bool {
        matches!(self.0, Principal::User)
    }
    pub(crate) fn from_source(source: &SourceInput) -> Self {
        Self(if source.is_cooperative() {
            Principal::Actor(source.client().into())
        } else {
            Principal::User
        })
    }
}
// Environment identity and captured connection, independent of user-visible alias/revision.
// Reimporting the same connection does not discard ownership; a different endpoint cannot inherit it.
type ResourceKey = (String, String, String);
#[derive(Default)]
pub(super) struct Resources {
    owners: BTreeMap<ResourceKey, Principal>,
    pending: usize,
}
fn max_resources() -> usize {
    wes_budgets::get("environment.resources") as usize
}

/// Reserved before dispatch; dropping a cancelled/failed creation releases only local capacity.
/// It never deletes a remote resource. A successful response supplies its exact identity.
pub struct ResourceCreation {
    authority: Authority,
    environment: String,
    connection: String,
    principal: Principal,
}
impl ResourceCreation {
    pub fn record(self, id: &str) -> Result<(), &'static str> {
        if id.is_empty() || id.len() > 256 {
            return Err("Invalid external resource identity");
        }
        let mut state = self
            .authority
            .0
            .write()
            .map_err(|_| "Resource authority unavailable")?;
        if !state.available(&self.environment) {
            return Err("Environment authority ended");
        }
        let key = (self.environment.clone(), self.connection.clone(), id.into());
        if state.resources.owners.contains_key(&key) {
            return Err("Creation response reused an existing resource identity");
        }
        state.resources.owners.insert(key, self.principal.clone());
        Ok(())
    }
}
impl Drop for ResourceCreation {
    fn drop(&mut self) {
        if let Ok(mut state) = self.authority.0.write() {
            state.resources.pending = state.resources.pending.saturating_sub(1);
        }
    }
}
impl Authority {
    pub fn reserve_resource(
        &self,
        caller: &InvocationAuthority,
        environment: &str,
        connection: &str,
    ) -> Result<ResourceCreation, &'static str> {
        let mut state = self
            .0
            .write()
            .map_err(|_| "Resource authority unavailable")?;
        if !state.available(environment) || matches!(caller.0, Principal::Unknown) {
            return Err(
                "External creation requires a live admitted user or actor and enabled environment",
            );
        }
        if connection.is_empty()
            || connection.len() > 8192
            || state.resources.owners.len() + state.resources.pending >= max_resources()
        {
            return Err("Live resource receipt capacity exhausted; no resource was created");
        }
        state.resources.pending += 1;
        Ok(ResourceCreation {
            authority: self.clone(),
            environment: environment.into(),
            connection: connection.into(),
            principal: caller.0.clone(),
        })
    }
    pub fn check_resource(
        &self,
        caller: &InvocationAuthority,
        environment: &str,
        connection: &str,
        id: &str,
    ) -> Result<(), &'static str> {
        let state = self
            .0
            .read()
            .map_err(|_| "Resource authority unavailable")?;
        if !state.available(environment) {
            return Err("Environment authority is disabled");
        }
        match &caller.0 {
            Principal::User => Ok(()),
            Principal::Actor(_)
                if state.resources.owners.get(&(
                    environment.into(),
                    connection.into(),
                    id.into(),
                )) == Some(&caller.0) =>
            {
                Ok(())
            }
            _ => Err(
                "This external resource is outside this actor's live creation scope. Public IDs, labels and work grants do not grant resource control. Use a container you created in this session and connection, or have the user operate on this exact ID.",
            ),
        }
    }
    pub fn forget_resource(&self, environment: &str, connection: &str, id: &str) {
        if let Ok(mut state) = self.0.write() {
            state
                .resources
                .owners
                .remove(&(environment.into(), connection.into(), id.into()));
        }
    }
    pub fn check_external_creation(
        &self,
        caller: &InvocationAuthority,
        environment: &str,
    ) -> Result<(), &'static str> {
        if !matches!(caller.0, Principal::Unknown) && self.available(environment) {
            Ok(())
        } else {
            Err("External creation requires a live admitted user or actor and enabled environment")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn actor(name: &str) -> InvocationAuthority {
        InvocationAuthority::from_source(
            &SourceInput::new("cell".into(), "".into())
                .unwrap()
                .with_client(name.into())
                .unwrap()
                .cooperative(),
        )
    }
    #[test]
    fn receipt_scope_is_connection_environment_and_live_principal_not_public_identity() {
        let authority = Authority::default();
        let a = actor("a");
        let b = actor("b");
        assert!(
            authority
                .reserve_resource(&InvocationAuthority::default(), "env", "socket")
                .is_err()
        );
        authority
            .reserve_resource(&a, "env", "socket")
            .unwrap()
            .record("id")
            .unwrap();
        assert!(authority.check_resource(&a, "env", "socket", "id").is_ok());
        for (caller, env, socket) in [
            (&b, "env", "socket"),
            (&a, "other", "socket"),
            (&a, "env", "other"),
        ] {
            assert!(authority.check_resource(caller, env, socket, "id").is_err());
        }
        assert!(
            authority
                .reserve_resource(&b, "env", "socket")
                .unwrap()
                .record("id")
                .is_err()
        );
        assert!(authority.check_resource(&a, "env", "socket", "id").is_ok());
        authority.restored();
        authority.enable("env".into()).unwrap();
        assert!(authority.check_resource(&a, "env", "socket", "id").is_err());
    }
    #[test]
    fn capacity_is_reserved_before_dispatch_and_released_on_abandonment() {
        let authority = Authority::default();
        let a = actor("a");
        let reservations: Vec<_> = (0..max_resources())
            .map(|_| authority.reserve_resource(&a, "env", "socket").unwrap())
            .collect();
        assert!(authority.reserve_resource(&a, "env", "socket").is_err());
        drop(reservations);
        assert!(authority.reserve_resource(&a, "env", "socket").is_ok());
        let pending = authority.reserve_resource(&a, "env", "socket").unwrap();
        authority.close();
        assert!(pending.record("id").is_err());
        assert!(authority.check_resource(&a, "env", "socket", "id").is_err());
    }
}
