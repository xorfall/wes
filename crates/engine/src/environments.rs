//! Single-owner environment publication kernel. Not a bypass for session write-ahead admission.
//!
//! Plans contain immutable, already captured inputs. This module performs no external I/O, provider
//! construction, credential lookup or execution. A future session handler must acknowledge its
//! durable declaration evidence before `apply`, and bound the number of retained plan/image handles.
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};
use wes_core::environments::{
    Binding, CapturedSources, EffectiveEnvironment, EnvironmentError, MAX_DEPTH, Package, Parent,
    Revision,
};

type Environments = BTreeMap<String, Arc<EffectiveEnvironment>>;
mod record;
pub use record::EnvironmentRecord;
mod authority;
mod execution;
pub use authority::{
    Authority, CredentialStatus, DispatchDenied, InvocationAuthority, ResourceCreation,
};
pub(crate) use execution::ExecutionImages;
pub use execution::{DefinitionEdit, EnvironmentDocument, EnvironmentLoader, LoadedDefinitions};
type Revisions = BTreeMap<(String, Revision), Arc<EffectiveEnvironment>>;
fn error(code: &'static str, message: impl Into<String>) -> EnvironmentError {
    EnvironmentError {
        code,
        message: message.into(),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub revisions: usize,
    /// Conservative canonical closure bytes, not RSS or memory retained by external handles.
    pub history_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            revisions: wes_budgets::get("environment.history.entries") as usize,
            history_bytes: wes_budgets::get("environment.history.bytes") as usize,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Selection {
    owner: Arc<()>,
    name: String,
    revision: Revision,
}
impl PartialEq for Selection {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner)
            && self.name == other.name
            && self.revision == other.revision
    }
}
impl Eq for Selection {}
impl Selection {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn revision(&self) -> Revision {
        self.revision
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub name: String,
    pub before: Option<Revision>,
    pub after: Option<Revision>,
    pub added_imports: Vec<String>,
    pub removed_imports: Vec<String>,
    /// Includes recipe/origin, captured evidence, resolved configuration and secret-reference changes.
    pub rebound_imports: Vec<String>,
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.name)?;
        match (self.before, self.after) {
            (None, Some(after)) => write!(f, "new (revision {after})")?,
            (Some(before), Some(after)) => write!(f, "revision {before} → {after}")?,
            (Some(before), None) => write!(f, "removed (revision {before})")?,
            (None, None) => write!(f, "unchanged")?,
        }
        for (label, imports) in [
            ("added", &self.added_imports),
            ("removed", &self.removed_imports),
            ("rebound", &self.rebound_imports),
        ] {
            if !imports.is_empty() {
                write!(f, "; imports {label}: {}", imports.join(", "))?;
            }
        }
        Ok(())
    }
}

/// A non-serializable owner capability, not an ordinary record that a user can fabricate.
pub struct Plan {
    owner: Arc<()>,
    epoch: u64,
    active: Environments,
    history: Revisions,
    changes: Vec<Change>,
    configured: Environments,
}
impl Plan {
    pub fn environments(&self) -> impl Iterator<Item = &Arc<EffectiveEnvironment>> {
        self.active.values()
    }
    pub fn changes(&self) -> &[Change] {
        &self.changes
    }
    pub fn inspect(&self, name: &str) -> Option<&Arc<EffectiveEnvironment>> {
        self.active.get(name)
    }
}
impl fmt::Debug for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvironmentPlan")
            .field("epoch", &self.epoch)
            .field("changes", &self.changes)
            .finish_non_exhaustive()
    }
}

/// In-process immutable reconstruction image, deliberately distinct from a plan capability.
/// This is not yet a portable or durable wire format, nor a source of run authorization.
#[derive(Clone)]
pub struct Image {
    active: Environments,
    history: Revisions,
    configured: Environments,
}
impl fmt::Debug for Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvironmentImage")
            .field("active", &self.active.len())
            .field("revisions", &self.history.len())
            .finish()
    }
}

pub struct Registry {
    owner: Arc<()>,
    epoch: u64,
    active: Environments,
    history: Revisions,
    limits: Limits,
    configured: Environments,
}
impl Default for Registry {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}
impl Registry {
    pub(crate) fn planning_snapshot(&self) -> Self {
        Self {
            owner: self.owner.clone(),
            epoch: self.epoch,
            active: self.active.clone(),
            history: self.history.clone(),
            limits: self.limits,
            configured: self.configured.clone(),
        }
    }
    pub fn revisions(&self) -> BTreeMap<String, Revision> {
        self.active
            .iter()
            .map(|(n, e)| (n.clone(), e.revision()))
            .collect()
    }
    pub(crate) fn document_token(&self) -> String {
        let revisions: Vec<_> = self
            .revisions()
            .into_iter()
            .map(|(name, revision)| format!("{name}={revision}"))
            .collect();
        Revision::evidence(
            "environment-editor/v1",
            revisions.iter().map(String::as_str),
        )
        .to_string()
    }
    /// Mutable definition history excludes application configuration. Neither grants nor
    /// application-owned providers are smuggled into user definition journals.
    pub(crate) fn recorded_revisions(&self) -> BTreeMap<String, Revision> {
        self.revisions()
            .into_iter()
            .filter(|(n, _)| !self.configured.contains_key(n))
            .collect()
    }
    pub(crate) fn configure(
        &mut self,
        environment: Arc<EffectiveEnvironment>,
    ) -> Result<(), EnvironmentError> {
        let name = environment.name().to_owned();
        if self.active.contains_key(&name) && !self.configured.contains_key(&name) {
            return Err(error(
                "ENV005",
                "configured environment cannot replace user definitions",
            ));
        }
        let mut history = self.history.clone();
        history.insert((name.clone(), environment.revision()), environment.clone());
        validate_history(&history, self.limits)?;
        let next = self
            .epoch
            .checked_add(1)
            .ok_or_else(|| error("ENV007", "environment revision exhausted"))?;
        self.history = history;
        self.active.insert(name.clone(), environment.clone());
        self.configured.insert(name, environment);
        self.epoch = next;
        Ok(())
    }
    pub fn validate_plan(&self, plan: &Plan) -> Result<(), EnvironmentError> {
        if !Arc::ptr_eq(&self.owner, &plan.owner) || self.epoch != plan.epoch {
            return Err(error(
                "ENV008",
                "environment plan belongs to another owner or an obsolete registry revision",
            ));
        }
        Ok(())
    }
    pub fn new(limits: Limits) -> Self {
        Self {
            owner: Arc::new(()),
            epoch: 0,
            active: BTreeMap::new(),
            history: BTreeMap::new(),
            limits,
            configured: BTreeMap::new(),
        }
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.active.keys().map(String::as_str)
    }
    pub fn inspect(&self, name: &str) -> Option<&Arc<EffectiveEnvironment>> {
        self.active.get(name)
    }
    pub fn retained_revision(
        &self,
        name: &str,
        revision: Revision,
    ) -> Option<&Arc<EffectiveEnvironment>> {
        self.history.get(&(name.into(), revision))
    }
    pub fn revision_count(&self) -> usize {
        self.history.len()
    }
    pub fn select(&self, name: &str) -> Result<Selection, EnvironmentError> {
        if self.inspect(name).is_some_and(|e| e.is_retired()) {
            return Err(error("ENV005", "environment is retired"));
        }
        let environment = self
            .active
            .get(name)
            .ok_or_else(|| error("ENV005", "environment is not available for selection"))?;
        Ok(Selection {
            owner: self.owner.clone(),
            name: name.into(),
            revision: environment.revision(),
        })
    }
    /// Resolves against an acknowledged revision without granting execution or secret authority.
    pub fn bind(&self, selection: &Selection, alias: &str) -> Result<Binding, EnvironmentError> {
        if !Arc::ptr_eq(&selection.owner, &self.owner) {
            return Err(error(
                "ENV008",
                "environment selection belongs to another registry owner",
            ));
        }
        let environment = self
            .active
            .get(selection.name())
            .ok_or_else(|| error("ENV005", "selected environment is no longer available"))?;
        if environment.revision() != selection.revision() {
            return Err(error(
                "ENV008",
                "selected environment revision changed; inspect and select again",
            ));
        }
        environment.bind(alias)
    }

    /// Prepare a complete registry image. Future multi-file ownership/drift reconciliation belongs
    /// above this boundary; omission removes only active visibility, never retained evidence/bindings.
    pub fn plan(
        &self,
        package: &Package,
        sources: &CapturedSources,
    ) -> Result<Plan, EnvironmentError> {
        self.plan_inner(package, sources, false)
    }
    pub(crate) fn plan_document(
        &self,
        package: &Package,
        sources: &CapturedSources,
    ) -> Result<Plan, EnvironmentError> {
        self.plan_inner(package, sources, true)
    }
    pub(crate) fn is_configured(&self, name: &str) -> bool {
        self.configured.contains_key(name)
    }
    fn plan_inner(
        &self,
        package: &Package,
        sources: &CapturedSources,
        edit_configured: bool,
    ) -> Result<Plan, EnvironmentError> {
        sources.validate(package)?;
        if !edit_configured
            && package
                .definitions()
                .keys()
                .any(|name| self.configured.contains_key(name))
        {
            return Err(error(
                "ENV005",
                "application-owned environments cannot be replaced; choose another name",
            ));
        }
        validate_history(&self.history, self.limits)?;
        let used_bytes: usize = self
            .history
            .iter()
            .map(|((name, _), environment)| name.len() + environment.charge())
            .sum();
        let mut resolver = Resolver {
            package,
            sources,
            history: &self.history,
            active: BTreeMap::new(),
            visiting: BTreeSet::new(),
            work: 0,
            remaining_bytes: self.limits.history_bytes - used_bytes,
            remaining_revisions: self.limits.revisions - self.history.len(),
        };
        for name in package.definitions().keys() {
            resolver.resolve(name)?;
        }
        let mut active = resolver.active;
        let configured: Environments = self
            .configured
            .iter()
            .filter(|(name, _)| !package.definitions().contains_key(*name))
            .map(|(name, environment)| (name.clone(), environment.clone()))
            .collect();
        active.extend(configured.clone());
        let mut identities = BTreeSet::new();
        for (name, environment) in &active {
            if self
                .active
                .get(name)
                .is_some_and(|old| old.is_retired() && !old.same_execution(environment))
            {
                return Err(error(
                    "ENV005",
                    "retired definitions are immutable; existing historical bindings remain available",
                ));
            }
            if self.history.values().any(|old| {
                old.is_retired()
                    && old.identity() == environment.identity()
                    && (!environment.is_retired() || old.name() != environment.name())
            }) {
                return Err(error(
                    "ENV005",
                    "retired identities cannot be resurrected or renamed; create a new explicit id",
                ));
            }
            if environment.parent().is_some_and(|parent| {
                parent.is_retired()
                    && self
                        .active
                        .get(name)
                        .and_then(|old| old.parent())
                        .is_none_or(|old| old.identity() != parent.identity())
            }) {
                return Err(error(
                    "ENV005",
                    "cannot add new inheritance references to a retired environment",
                ));
            }
            if !identities.insert(environment.identity()) {
                return Err(error(
                    "ENV005",
                    "environment identities must be unique; rename must replace the old display name",
                ));
            }
            if self
                .active
                .get(name)
                .is_some_and(|old| old.identity() != environment.identity())
            {
                return Err(error(
                    "ENV005",
                    "an existing environment's stable identity cannot be replaced",
                ));
            }
        }
        let changes = changes(&self.active, &active);
        let mut history = self.history.clone();
        for (name, environment) in &active {
            history
                .entry((name.clone(), environment.revision()))
                .or_insert_with(|| environment.clone());
        }
        validate_history(&history, self.limits)?;
        if !changes.is_empty() && self.epoch == u64::MAX {
            return Err(error(
                "ENV007",
                "environment publication revision exhausted",
            ));
        }
        Ok(Plan {
            owner: self.owner.clone(),
            epoch: self.epoch,
            active,
            history,
            changes,
            configured,
        })
    }

    /// The session coordinator must obtain any required journal acknowledgment before calling this.
    /// No allocation-dependent validation or fallible operation follows the first state mutation.
    pub fn apply(&mut self, plan: Plan) -> Result<Vec<Change>, EnvironmentError> {
        self.validate_plan(&plan)?;
        if plan.changes.is_empty() {
            return Ok(plan.changes);
        }
        let next = self
            .epoch
            .checked_add(1)
            .ok_or_else(|| error("ENV007", "environment publication revision exhausted"))?;
        self.active = plan.active;
        self.history = plan.history;
        self.configured = plan.configured;
        self.epoch = next;
        Ok(plan.changes)
    }
    pub fn image(&self) -> Image {
        Image {
            active: self.active.clone(),
            history: self.history.clone(),
            configured: self.configured.clone(),
        }
    }
    pub fn restore(image: Image, limits: Limits) -> Result<Self, EnvironmentError> {
        validate_history(&image.history, limits)?;
        Ok(Self {
            owner: Arc::new(()),
            epoch: 0,
            active: image.active,
            history: image.history,
            limits,
            configured: image.configured,
        })
    }
}
impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvironmentRegistry")
            .field("epoch", &self.epoch)
            .field("environments", &self.active.len())
            .field("revisions", &self.history.len())
            .finish()
    }
}

fn validate_history(history: &Revisions, limits: Limits) -> Result<(), EnvironmentError> {
    if history.len() > limits.revisions {
        return Err(error(
            "ENV007",
            "retained environment revision count exceeds its budget",
        ));
    }
    let mut remaining = limits.history_bytes;
    for ((name, _), environment) in history {
        remaining = remaining
            .checked_sub(name.len())
            .and_then(|r| r.checked_sub(environment.charge()))
            .ok_or_else(|| {
                error(
                    "ENV007",
                    "retained environment closure bytes exceed their budget",
                )
            })?;
    }
    Ok(())
}
struct Resolver<'a> {
    package: &'a Package,
    sources: &'a CapturedSources,
    history: &'a Revisions,
    active: Environments,
    visiting: BTreeSet<String>,
    work: usize,
    remaining_bytes: usize,
    remaining_revisions: usize,
}
impl Resolver<'_> {
    fn resolve(&mut self, name: &str) -> Result<Arc<EffectiveEnvironment>, EnvironmentError> {
        if let Some(environment) = self.active.get(name) {
            return Ok(environment.clone());
        }
        self.work += 1;
        if self.work > 4096 || self.visiting.len() >= MAX_DEPTH {
            return Err(error(
                "ENV007",
                "environment resolution work/depth budget exceeded",
            ));
        }
        if !self.visiting.insert(name.into()) {
            return Err(error(
                "ENV002",
                format!("environment inheritance cycle at: {name}"),
            ));
        }
        let definition = self
            .package
            .definitions()
            .get(name)
            .ok_or_else(|| error("ENV002", "inherited environment does not exist"))?;
        let parent_reference = definition.parent.clone();
        let parent = match &parent_reference {
            None => None,
            Some(Parent::Latest(parent)) => Some(self.resolve(parent)?),
            Some(Parent::Pinned {
                name: parent,
                revision,
            }) => {
                if parent == name {
                    return Err(error("ENV002", "an environment cannot inherit itself"));
                }
                if let Some(pinned) = self.history.get(&(parent.clone(), *revision)) {
                    Some(pinned.clone())
                } else {
                    let candidate = self.resolve(parent)?;
                    if candidate.revision() != *revision {
                        return Err(error(
                            "ENV002",
                            "pinned parent revision is unavailable; no fallback to latest",
                        ));
                    }
                    Some(candidate)
                }
            }
        };
        let environment = Arc::new(EffectiveEnvironment::resolve(
            self.package,
            name,
            parent,
            self.sources,
        )?);
        let environment =
            if let Some(previous) = self.history.get(&(name.into(), environment.revision())) {
                previous.clone()
            } else {
                self.remaining_bytes = self
                    .remaining_bytes
                    .checked_sub(name.len())
                    .and_then(|b| b.checked_sub(environment.charge()))
                    .ok_or_else(|| {
                        error(
                            "ENV007",
                            "retained environment closure bytes exceed their budget",
                        )
                    })?;
                self.remaining_revisions =
                    self.remaining_revisions.checked_sub(1).ok_or_else(|| {
                        error(
                            "ENV007",
                            "retained environment revision count exceeds its budget",
                        )
                    })?;
                environment
            };
        self.visiting.remove(name);
        self.active.insert(name.into(), environment.clone());
        Ok(environment)
    }
}
fn changes(before: &Environments, after: &Environments) -> Vec<Change> {
    before
        .keys()
        .chain(after.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|name| {
            let old = before.get(name);
            let new = after.get(name);
            if old.map(|e| e.revision()) == new.map(|e| e.revision()) {
                return None;
            }
            let old_imports = old.map(|e| e.imports());
            let new_imports = new.map(|e| e.imports());
            let added_imports = new_imports
                .into_iter()
                .flat_map(|m| m.keys())
                .filter(|key| old_imports.is_none_or(|m| !m.contains_key(*key)))
                .cloned()
                .collect();
            let removed_imports = old_imports
                .into_iter()
                .flat_map(|m| m.keys())
                .filter(|key| new_imports.is_none_or(|m| !m.contains_key(*key)))
                .cloned()
                .collect();
            let rebound_imports = new_imports
                .into_iter()
                .flat_map(|m| m.iter())
                .filter(|(key, value)| {
                    old_imports
                        .and_then(|m| m.get(*key))
                        .is_some_and(|old| old != *value)
                })
                .map(|(key, _)| key.clone())
                .collect();
            Some(Change {
                name: name.clone(),
                before: old.map(|e| e.revision()),
                after: new.map(|e| e.revision()),
                added_imports,
                removed_imports,
                rebound_imports,
            })
        })
        .collect()
}
