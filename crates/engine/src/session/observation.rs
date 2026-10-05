//! Coherent read boundary for clients. No wire types, effects, history reads or credentials.
use super::{Actor, LogSnapshot, SessionSnapshot, SourceInput, SubmissionReply, ValueSnapshot};
use crate::driver::Snapshot;
use wes_core::capability::Catalogue;
use wes_language::templates::Templates;

#[derive(Clone, Debug)]
pub struct ObservedCell {
    pub input: SourceInput,
    /// None is still pending. Recovered replies retain their explicit `restored` status.
    pub reply: Option<SubmissionReply>,
}

#[derive(Clone, Debug)]
pub struct SessionObservation {
    /// Read-only live evidence handle; each get captures independently of this metadata snapshot.
    pub traces: crate::trace::Traces,
    pub interactive_providers: std::collections::BTreeMap<Option<String>, Vec<String>>,
    pub saved_workspaces: std::sync::Arc<[crate::workspace::WorkspaceName]>,
    pub environment_plans:
        std::collections::BTreeMap<(String, String), Vec<crate::environments::Change>>,
    pub environment_managed: bool,
    pub default_environment: Option<String>,
    pub environment_enabled: std::collections::BTreeMap<String, bool>,
    pub environment_credentials: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    >,
    pub environment_revisions: std::collections::BTreeMap<String, wes_core::environments::Revision>,
    pub environment_clients:
        std::collections::BTreeMap<String, wes_core::environments::EnvironmentContext>,
    pub execution_targets: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<
            String,
            Result<
                std::sync::Arc<wes_core::environments::Target>,
                wes_core::environments::EnvironmentError,
            >,
        >,
    >,
    pub environment_providers: std::collections::BTreeMap<
        String,
        std::collections::BTreeMap<String, (String, String, Option<String>)>,
    >,
    pub environment_catalogues: std::collections::BTreeMap<String, Catalogue>,
    pub state: SessionSnapshot,
    pub cells: Vec<ObservedCell>,
    pub catalogue: Catalogue,
    pub templates: Templates,
    pub types: Vec<String>,
    pub views: wes_views::Catalogue,
    pub importers: Vec<String>,
    pub importer_parameters: indexmap::IndexMap<String, Vec<wes_core::capability::Parameter>>,
    pub log: LogSnapshot,
    pub values: Option<ValueSnapshot>,
}

impl SessionObservation {
    /// Resolve work membership independently of presentation order. A durable command
    /// may precede a missing submission receipt after a crash. Memoized paths keep
    /// arbitrarily long revision chains within the bounded cell inventory.
    pub fn work_roots(&self) -> std::collections::BTreeMap<&str, &str> {
        use std::collections::{BTreeMap, BTreeSet};
        let inputs: BTreeMap<_, _> = self
            .cells
            .iter()
            .map(|c| (c.input.cell(), &c.input))
            .collect();
        let mut roots = BTreeMap::new();
        for cell in &self.cells {
            let mut cursor = cell.input.cell();
            let mut path = BTreeSet::new();
            let root = loop {
                if let Some(root) = roots.get(cursor) {
                    break *root;
                }
                if !path.insert(cursor) {
                    break cursor;
                }
                match inputs
                    .get(cursor)
                    .and_then(|i| i.parent())
                    .filter(|p| inputs.contains_key(*p))
                {
                    Some(parent) => cursor = parent,
                    None => break cursor,
                }
            };
            for id in path {
                roots.insert(id, root);
            }
        }
        roots
    }
}

impl Actor {
    pub(super) fn observation(&self) -> SessionObservation {
        SessionObservation {
            traces: self.workspace.traces.clone(),
            views: self.workspace.view_catalogue().clone(),
            types: crate::tasks::type_completion_names(self.workspace.contracts()),
            interactive_providers: std::iter::once((
                None,
                self.workspace.interactive_provider_names(),
            ))
            .chain(
                self.workspace
                    .environments()
                    .revisions()
                    .into_iter()
                    .filter_map(|(name, revision)| {
                        self.workspace
                            .environment_images
                            .get(&name, revision)
                            .map(|p| (Some(name), p.interactive_names()))
                    }),
            )
            .collect(),
            saved_workspaces: self.workspace.saved_workspace_names(),
            environment_plans: self
                .environment_plans
                .iter()
                .map(|(key, plan)| (key.clone(), plan.changes().to_vec()))
                .collect(),
            environment_managed: self.workspace.environment_managed,
            default_environment: self.workspace.default_environment.clone(),
            environment_enabled: self
                .workspace
                .environments()
                .names()
                .map(|name| {
                    (
                        name.into(),
                        self.workspace
                            .environment_loader
                            .as_ref()
                            .and_then(|l| l.authority())
                            .is_some_and(|a| {
                                self.workspace
                                    .environments()
                                    .inspect(name)
                                    .is_some_and(|e| a.available(e.identity()))
                            }),
                    )
                })
                .collect(),
            environment_credentials: self
                .workspace
                .environments()
                .names()
                .filter_map(|name| {
                    self.workspace.environments().inspect(name).map(|e| {
                        (
                            name.into(),
                            e.imports()
                                .iter()
                                .map(|(alias, import)| {
                                    (alias.clone(), import.credential_refs().clone())
                                })
                                .collect(),
                        )
                    })
                })
                .collect(),
            environment_revisions: self.workspace.environments().revisions(),
            environment_clients: self.environment_clients.clone(),
            execution_targets: self
                .workspace
                .environments()
                .revisions()
                .keys()
                .filter_map(|name| {
                    self.workspace
                        .environments()
                        .inspect(name)
                        .filter(|env| !env.is_retired() && !env.is_abstract())
                        .map(|env| (name.clone(), env.execution_targets()))
                })
                .collect(),
            environment_providers: self.workspace.provider_placements(),
            environment_catalogues: self
                .workspace
                .environments()
                .revisions()
                .into_iter()
                .filter_map(|(name, revision)| {
                    self.workspace
                        .environment_images
                        .get(&name, revision)
                        .map(|p| (name, p.catalogue().clone()))
                })
                .collect(),
            state: SessionSnapshot {
                execution: Snapshot::capture(self.workspace.runtime(), self.io.is_idle()),
                names: self.workspace.bindings().names().clone(),
                conversations: self.io.active_conversations(self.workspace.runtime()),
                admission_pending: self.active.is_some() || !self.environment_work.is_empty(),
                checkpoint_pending: self.checkpoints.busy(),
                recording_blocked: self.recording_failed,
                restoration: self.restoration.clone(),
            },
            cells: self.cells.observed(),
            catalogue: self.workspace.catalogue().clone(),
            templates: self.workspace.templates().clone(),
            importers: self.workspace.importer_names().map(str::to_owned).collect(),
            importer_parameters: self.workspace.importer_parameters().clone(),
            log: self.log.snapshot(),
            values: self.values.as_ref().map(super::SessionValues::snapshot),
        }
    }
}
