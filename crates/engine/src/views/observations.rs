//! Presentation leases suspend read demand, not read intent. Neither owns a source execution.
use super::*;
use tokio::time::{Duration, Instant};
const TTL: Duration = Duration::from_secs(3);
#[derive(Debug)]
pub(super) struct Mount {
    pub root: Handle,
    pub expires: Instant,
}
#[derive(Clone, Debug)]
pub enum MountAction {
    Open,
    Touch(String),
    Close(String),
    Start,
    Stop,
}
impl Store {
    pub fn open_mount(&mut self, root: &Handle) -> Result<String, Error> {
        self.expire_mounts();
        self.instance(root)?;
        if self.mounts.len() >= wes_budgets::get("ui.view.roots") as usize {
            return Err(Error::Capacity);
        }
        let token = uuid::Uuid::new_v4().to_string();
        self.mounts.insert(
            token.clone(),
            Mount {
                root: root.clone(),
                expires: Instant::now() + TTL,
            },
        );
        self.reconcile_mounts();
        Ok(token)
    }
    pub fn touch_mount(&mut self, root: &Handle, token: &str) -> Result<(), Error> {
        self.expire_mounts();
        self.instance(root)?;
        let mount = self
            .mounts
            .get_mut(token)
            .filter(|m| m.root.identity == root.identity)
            .ok_or(Error::DisplayClosed)?;
        mount.expires = Instant::now() + TTL;
        Ok(())
    }
    pub fn close_mount(&mut self, root: &Handle, token: &str) -> Result<(), Error> {
        if let Some(mount) = self.mounts.get(token) {
            if mount.root.identity != root.identity {
                return Err(Error::Reference);
            }
        }
        self.mounts.remove(token);
        self.reconcile_mounts();
        Ok(())
    }
    pub fn mount_deadline(&self) -> Option<Instant> {
        self.mounts.values().map(|m| m.expires).min()
    }
    pub fn expire_mounts(&mut self) {
        let now = Instant::now();
        let instances = &self.instances;
        self.mounts.retain(|_, m| {
            m.expires > now
                && instances
                    .get(&m.root.id)
                    .is_some_and(|i| i.handle.identity == m.root.identity)
        });
        self.reconcile_mounts();
    }
    pub(super) fn reconcile_mounts(&mut self) {
        let mut next = BTreeSet::new();
        let mut pending = self
            .mounts
            .values()
            .map(|m| m.root.id.clone())
            .collect::<Vec<_>>();
        while let Some(id) = pending.pop() {
            if let Some(instance) = self.instances.get(&id) {
                if next.insert(id) {
                    pending.extend(instance.snapshot.members.values().flatten().cloned());
                }
            }
        }
        for lost in self.visible.difference(&next) {
            if let Some(instance) = self.instances.get_mut(lost) {
                if instance.snapshot.observing {
                    instance.snapshot.observing = false;
                    instance.snapshot.input_revision =
                        instance.snapshot.input_revision.saturating_add(1);
                }
            }
        }
        for gained in next.difference(&self.visible) {
            if let Some(instance) = self.instances.get_mut(gained) {
                if instance.input_observation_requested && !instance.snapshot.observing {
                    instance.snapshot.observing = true;
                    instance.snapshot.input_revision =
                        instance.snapshot.input_revision.saturating_add(1);
                }
            }
        }
        self.visible = next;
    }
    pub(crate) fn included(&self, root: &Handle) -> Result<Vec<Handle>, Error> {
        self.instance(root)?;
        Ok(self
            .instances
            .values()
            .filter(|i| self.reaches(&root.id, &i.handle.id))
            .map(|i| i.handle.clone())
            .collect())
    }
    pub fn set_observing(&mut self, root: &Handle, active: bool) -> Result<(), Error> {
        let included = self.included(root)?;
        for handle in included {
            let i = self
                .instances
                .get_mut(&handle.id)
                .expect("included instance");
            if i.snapshot
                .input
                .as_ref()
                .is_some_and(|v| v.binding.current())
            {
                i.input_observation_requested = active;
            }
            if (i.snapshot.query.is_some() && !active
                || i.snapshot
                    .input
                    .as_ref()
                    .is_some_and(|v| v.binding.current()))
                && i.snapshot.observing != active
            {
                i.snapshot.observing = active;
                i.snapshot.input_problem = None;
                i.snapshot.input_revision = i.snapshot.input_revision.saturating_add(1);
            }
        }
        Ok(())
    }
    pub(crate) fn samples(
        &mut self,
        root: &Handle,
        token: &str,
    ) -> Result<Vec<(Handle, Source)>, Error> {
        self.touch_mount(root, token)?;
        Ok(self
            .included(root)?
            .into_iter()
            .filter_map(|h| {
                let i = &self.instances[&h.id];
                let input = i.snapshot.input.as_ref()?;
                (i.snapshot.observing && input.binding.current())
                    .then(|| input.source().cloned().map(|s| (h, s)))
                    .flatten()
            })
            .collect())
    }
    pub(crate) fn waiting(&mut self, handle: &Handle) {
        if let Some(i) = self.instances.get_mut(&handle.id) {
            let message = Some("Waiting for the current source result".to_string());
            if i.snapshot.input_problem != message {
                i.snapshot.input_problem = message;
                i.snapshot.input_revision = i.snapshot.input_revision.saturating_add(1);
            }
        }
    }
    pub(crate) fn sample_source(
        &mut self,
        handle: &Handle,
        value: Value,
        run: Option<crate::runtime::RunId>,
    ) -> Result<(), Error> {
        let digest = super::persistence::input_digest(&value);
        self.sample(handle, Some(value), None)?;
        if let Some(source) = self
            .instances
            .get_mut(&handle.id)
            .and_then(|i| i.snapshot.input.as_mut())
            .and_then(Input::source_mut)
        {
            source.run = run;
            source.digest = digest;
        }
        Ok(())
    }
    pub(crate) fn sample(
        &mut self,
        handle: &Handle,
        value: Option<Value>,
        problem: Option<String>,
    ) -> Result<(), Error> {
        let instance = self.instance(handle)?;
        let revision = instance
            .snapshot
            .input_revision
            .checked_add(1)
            .ok_or(Error::Capacity)?;
        let mut next = instance.snapshot.input.clone().ok_or(Error::Input)?;
        if let Some(value) = value {
            if next
                .value
                .as_ref()
                .is_some_and(|old| std::ptr::eq(value.data(), old.data()))
                && instance.snapshot.input_problem.is_none()
            {
                return Ok(());
            }
            next.value = Some(value);
            let charge =
                self.check_input(&instance.snapshot.definition, Some(&next), instance.charge)?;
            let instance = self
                .instances
                .get_mut(&handle.id)
                .expect("sampled instance");
            self.charged = self.charged - instance.charge + charge;
            instance.charge = charge;
            instance.snapshot.input = Some(next);
        }
        let instance = self
            .instances
            .get_mut(&handle.id)
            .expect("sampled instance");
        if problem.is_some() {
            instance.snapshot.observing = false;
            // A failed or revoked source needs an explicit decision, not a reopen retry.
            instance.input_observation_requested = false;
        }
        instance.snapshot.input_problem = problem;
        instance.snapshot.input_revision = revision;
        Ok(())
    }
}
impl crate::workspace::Workspace {
    pub(crate) fn has_stream_source(&self, node: &NodeId) -> bool {
        let mut pending = vec![node.clone()];
        let mut seen = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if seen.insert(id.clone()) {
                let Some(entry) = self.runtime().graph().node(&id) else {
                    return false;
                };
                if entry
                    .payload()
                    .call()
                    .is_some_and(crate::providers::BoundCall::streaming)
                {
                    return true;
                }
                pending.extend(entry.dependencies().keys().cloned());
            }
        }
        false
    }

    pub fn view_mount(
        &mut self,
        node: &NodeId,
        identity: &str,
        action: MountAction,
    ) -> Result<Option<String>, String> {
        let handle = self
            .views
            .instances
            .get(node)
            .filter(|i| i.handle.identity.as_ref() == identity)
            .ok_or("View identity changed")?
            .handle
            .clone();
        if !matches!(action, MountAction::Close(_)) {
            self.view_frame(node)?;
        }
        match action {
            MountAction::Open => self.views.open_mount(&handle).map(Some),
            MountAction::Touch(token) => self.views.touch_mount(&handle, &token).map(|()| None),
            MountAction::Close(token) => self.views.close_mount(&handle, &token).map(|()| None),
            MountAction::Start => self.views.set_observing(&handle, true).map(|()| None),
            MountAction::Stop => self.views.set_observing(&handle, false).map(|()| None),
        }
        .map_err(|e| e.to_string())
    }
    pub(crate) fn view_samples(
        &mut self,
        node: &NodeId,
        identity: &str,
        token: &str,
    ) -> Result<Vec<(Handle, Source)>, String> {
        self.view_frame(node)?;
        let handle = self
            .views
            .instances
            .get(node)
            .filter(|i| i.handle.identity.as_ref() == identity)
            .ok_or("View identity changed")?
            .handle
            .clone();
        self.views
            .samples(&handle, token)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (Store, Handle) {
        let mut store = Store::default();
        let package = store.catalogue().get("Metric").unwrap().clone();
        let value = Value::new(
            package.input().shape(),
            Data::Record(
                [
                    ("view".into(), Data::Text("metric".into())),
                    ("value".into(), Data::Int(1)),
                ]
                .into(),
            ),
            wes_core::Provenance::default(),
        )
        .unwrap();
        let input = Input::from_source(
            Source::new(
                OutputRef::data(NodeId::new("source").unwrap()),
                Arc::new(()),
            ),
            value,
        );
        let handle = store
            .create(
                NodeId::new("card").unwrap(),
                "Metric",
                &package.digest,
                Some(input),
            )
            .unwrap();
        (store, handle)
    }
    #[test]
    fn sampled_failure_revokes_intent_and_reopening_does_not_retry() {
        let (mut store, handle) = setup();
        let token = store.open_mount(&handle).unwrap();
        store.set_observing(&handle, true).unwrap();
        store
            .sample(&handle, None, Some("Source unavailable".into()))
            .unwrap();
        store.close_mount(&handle, &token).unwrap();
        store.open_mount(&handle).unwrap();
        let frame = store.read(&handle).unwrap();
        assert!(!frame.observing);
        assert_eq!(frame.input_problem.as_deref(), Some("Source unavailable"));
    }
    #[test]
    fn current_rebind_restores_read_intent_and_rejected_rebind_preserves_it() {
        let (mut store, handle) = setup();
        let token = store.open_mount(&handle).unwrap();
        store.set_observing(&handle, true).unwrap();
        assert_eq!(store.bind(&handle, 99, None), Err(Error::Revision));
        store.close_mount(&handle, &token).unwrap();
        let token = store.open_mount(&handle).unwrap();
        assert!(store.read(&handle).unwrap().observing);
        let input = store.read(&handle).unwrap().input;
        store.bind(&handle, 0, input).unwrap();
        store.close_mount(&handle, &token).unwrap();
        store.open_mount(&handle).unwrap();
        assert!(store.read(&handle).unwrap().observing);
        let value = store
            .read(&handle)
            .unwrap()
            .input
            .unwrap()
            .value()
            .unwrap()
            .clone();
        store
            .bind(&handle, 1, Some(Input::constant(value)))
            .unwrap();
        assert!(!store.read(&handle).unwrap().observing);
    }
}
